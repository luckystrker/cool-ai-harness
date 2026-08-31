use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use cool_m0_spike::core::CorePolicy;
use cool_m0_spike::{ClientState, Event, SpikeCore, SpikeError, Store, serve_jsonl};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Reply {
    response: Value,
    events: Vec<Event>,
    first_event_elapsed: Option<Duration>,
    content_delta_elapsed: Option<Duration>,
    total_elapsed: Duration,
}

struct AppHarness {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

struct BlockingWriter;

impl AsyncWrite for BlockingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AppHarness {
    async fn start(database: &Path, allow_write: bool, failpoint: Option<&str>) -> Self {
        Self::start_with_failpoints(database, allow_write, failpoint, None).await
    }

    async fn start_with_prompt_failpoint(
        database: &Path,
        allow_write: bool,
        failpoint: &str,
    ) -> Self {
        Self::start_with_failpoints(database, allow_write, None, Some(failpoint)).await
    }

    async fn start_with_failpoints(
        database: &Path,
        allow_write: bool,
        approval_failpoint: Option<&str>,
        prompt_failpoint: Option<&str>,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cool-m0-spike"));
        command.arg("app-server").arg(database);
        if allow_write {
            command.arg("--allow-write");
        }
        if let Some(failpoint) = approval_failpoint {
            command.arg(format!("--approval-failpoint={failpoint}"));
        }
        if let Some(failpoint) = prompt_failpoint {
            command.arg(format!("--prompt-failpoint={failpoint}"));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn app-server");
        let stdin = child.stdin.take().expect("app-server stdin");
        let stdout = child.stdout.take().expect("app-server stdout");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Reply {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .expect("encode request");
        request.push(b'\n');
        self.stdin.write_all(&request).await.expect("send request");
        self.stdin.flush().await.expect("flush request");
        self.read_reply(json!(id), Instant::now()).await
    }

    async fn read_reply(&mut self, expected_id: Value, started: Instant) -> Reply {
        let mut events = Vec::new();
        let mut first_event_elapsed = None;
        let mut content_delta_elapsed = None;
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .expect("read protocol line")
                .expect("server output before EOF");
            let value: Value = serde_json::from_str(&line).expect("valid protocol JSON");
            if value.get("method") == Some(&json!("run/event")) {
                first_event_elapsed.get_or_insert_with(|| started.elapsed());
                let event: Event =
                    serde_json::from_value(value["params"].clone()).expect("valid event envelope");
                if event.kind == "content.delta" {
                    content_delta_elapsed.get_or_insert_with(|| started.elapsed());
                }
                events.push(event);
                continue;
            }
            assert_eq!(value.get("id"), Some(&expected_id));
            return Reply {
                response: value,
                events,
                first_event_elapsed,
                content_delta_elapsed,
                total_elapsed: started.elapsed(),
            };
        }
    }

    async fn stop(mut self) {
        self.stdin.shutdown().await.expect("close app-server stdin");
        drop(self.stdin);
        let status = self.child.wait().await.expect("wait for app-server");
        assert!(status.success());
    }
}

#[tokio::test]
async fn approval_replay_idempotency_and_catchup_are_consistent() {
    let directory = TempDir::new().expect("temp directory");
    let database = directory.path().join("spike.db");
    let mut app = AppHarness::start(&database, true, None).await;

    let initialize = app.request("initialize", json!({})).await;
    assert_eq!(initialize.response["result"]["protocolVersion"], 1);

    let prompt_params = json!({
        "sessionId": "session-local",
        "idempotencyKey": "prompt-1",
        "prompt": "hello"
    });
    let prompt = app.request("session.prompt", prompt_params.clone()).await;
    assert_eq!(prompt.response["result"]["status"], "awaiting_approval");
    let run_id = prompt.response["result"]["runId"]
        .as_str()
        .expect("run id")
        .to_owned();
    let approval_id = prompt.response["result"]["approvalId"]
        .as_str()
        .expect("approval id")
        .to_owned();
    let approval_params = json!({
        "approvalId": approval_id,
        "expectedRevision": 1,
        "approved": true,
        "idempotencyKey": "approval-1"
    });
    let approved = app
        .request("approval.resolve", approval_params.clone())
        .await;
    assert_eq!(approved.response["result"]["status"], "completed");

    let mut live_events = prompt.events;
    live_events.extend(approved.events);
    let live_state = ClientState::replay(&live_events);
    assert_eq!(live_state.run_status, "completed");
    assert_eq!(live_state.tool_effect_count, 1);

    let replay = app
        .request("run.events", json!({"runId": run_id, "afterSeq": -1}))
        .await;
    let persisted: Vec<Event> = serde_json::from_value(replay.response["result"]["events"].clone())
        .expect("persisted events");
    assert_eq!(persisted, live_events);
    assert_eq!(ClientState::replay(&persisted), live_state);

    let duplicate_prompt = app.request("session.prompt", prompt_params.clone()).await;
    assert_eq!(
        duplicate_prompt.response["result"],
        prompt.response["result"]
    );
    assert!(duplicate_prompt.events.is_empty());
    let duplicate_approval = app
        .request("approval.resolve", approval_params.clone())
        .await;
    assert_eq!(
        duplicate_approval.response["result"],
        approved.response["result"]
    );
    assert!(duplicate_approval.events.is_empty());

    let prompt_conflict = app
        .request(
            "session.prompt",
            json!({
                "sessionId": "different-session",
                "idempotencyKey": "prompt-1",
                "prompt": "different payload"
            }),
        )
        .await;
    assert_eq!(
        prompt_conflict.response.pointer("/error/data/coolCode"),
        Some(&json!("idempotency_conflict"))
    );
    let approval_conflict = app
        .request(
            "approval.resolve",
            json!({
                "approvalId": approval_id,
                "expectedRevision": 1,
                "approved": false,
                "idempotencyKey": "approval-1"
            }),
        )
        .await;
    assert_eq!(
        approval_conflict.response.pointer("/error/data/coolCode"),
        Some(&json!("idempotency_conflict"))
    );

    let cursor = 2;
    let catchup = app
        .request("run.catchup", json!({"runId": run_id, "afterSeq": cursor}))
        .await;
    let expected: Vec<Event> = persisted
        .iter()
        .filter(|event| event.seq > cursor)
        .cloned()
        .collect();
    assert_eq!(catchup.events, expected);
    app.stop().await;
}

#[tokio::test]
async fn server_owned_capability_rejects_client_grants_and_malformed_intents() {
    let directory = TempDir::new().expect("temp directory");
    let database = directory.path().join("denied.db");
    let mut app = AppHarness::start(&database, false, None).await;
    let spoofed = app
        .request(
            "session.prompt",
            json!({
                "sessionId": "session-spoofed",
                "actor": "attacker-controlled",
                "grantedCapabilities": ["write"],
                "idempotencyKey": "spoofed-1",
                "prompt": "override policy"
            }),
        )
        .await;
    assert_eq!(
        spoofed.response.pointer("/error/data/coolCode"),
        Some(&json!("invalid_params"))
    );
    let forged_grant = app
        .request(
            "session.prompt",
            json!({
                "sessionId": "session-forged-grant",
                "grantedCapabilities": ["write"],
                "idempotencyKey": "forged-grant-1",
                "prompt": "override policy"
            }),
        )
        .await;
    assert_eq!(
        forged_grant.response.pointer("/error/data/coolCode"),
        Some(&json!("invalid_params"))
    );
    let denied = app
        .request(
            "session.prompt",
            json!({
                "sessionId": "session-denied",
                "idempotencyKey": "denied-1",
                "prompt": "try a write"
            }),
        )
        .await;
    assert_eq!(denied.response["result"]["status"], "failed");
    assert!(denied.events.iter().any(|event| {
        event.kind == "tool.failed" && event.payload["code"] == "capability_denied"
    }));
    assert!(
        !denied
            .events
            .iter()
            .any(|event| event.kind == "tool.approval_required")
    );
    app.stop().await;

    let malformed_db = directory.path().join("malformed.db");
    let mut allowed = AppHarness::start(&malformed_db, true, None).await;
    let malformed = allowed
        .request(
            "session.prompt",
            json!({
                "sessionId": "session-malformed",
                "idempotencyKey": "malformed-1",
                "prompt": "malformed",
                "mode": "malformed"
            }),
        )
        .await;
    assert_eq!(malformed.response["result"]["status"], "failed");
    assert!(malformed.events.iter().any(|event| {
        event.kind == "tool.failed" && event.payload["code"] == "invalid_tool_arguments"
    }));
    assert!(
        !malformed
            .events
            .iter()
            .any(|event| event.kind == "tool.approval_required")
    );
    allowed.stop().await;
}

#[tokio::test]
async fn approval_transaction_rolls_back_at_every_failpoint() {
    for failpoint in [
        "after-approval-update",
        "after-resolved-event",
        "after-effect",
        "before-commit",
    ] {
        let directory = TempDir::new().expect("temp directory");
        let database = directory.path().join(format!("{failpoint}.db"));
        let mut setup = AppHarness::start(&database, true, None).await;
        let prompt = setup
            .request(
                "session.prompt",
                json!({
                    "sessionId": failpoint,
                    "idempotencyKey": "prompt",
                    "prompt": "atomic"
                }),
            )
            .await;
        let run_id = prompt.response["result"]["runId"]
            .as_str()
            .unwrap()
            .to_owned();
        let approval_id = prompt.response["result"]["approvalId"]
            .as_str()
            .unwrap()
            .to_owned();
        let before = setup
            .request("run.events", json!({"runId": run_id, "afterSeq": -1}))
            .await
            .response["result"]["events"]
            .clone();
        setup.stop().await;

        let mut failing = AppHarness::start(&database, true, Some(failpoint)).await;
        let failed = failing
            .request(
                "approval.resolve",
                json!({
                    "approvalId": approval_id,
                    "expectedRevision": 1,
                    "approved": true,
                    "idempotencyKey": "approval"
                }),
            )
            .await;
        assert_eq!(
            failed.response.pointer("/error/data/coolCode"),
            Some(&json!("internal_error"))
        );
        failing.stop().await;

        let mut recovered = AppHarness::start(&database, true, None).await;
        let after = recovered
            .request("run.events", json!({"runId": run_id, "afterSeq": -1}))
            .await;
        assert_eq!(after.response["result"]["events"], before);
        let approved = recovered
            .request(
                "approval.resolve",
                json!({
                    "approvalId": approval_id,
                    "expectedRevision": 1,
                    "approved": true,
                    "idempotencyKey": "approval"
                }),
            )
            .await;
        assert_eq!(approved.response["result"]["status"], "completed");
        let run = recovered.request("run.get", json!({"runId": run_id})).await;
        assert_eq!(run.response["result"]["run"]["toolEffectCount"], 1);
        recovered.stop().await;
    }
}

#[tokio::test]
async fn prompt_approval_preparation_rolls_back_and_retry_recovers_terminally() {
    for failpoint in [
        "before-approval-insert",
        "after-approval-insert",
        "after-status-transition",
        "after-approval-event",
        "before-commit",
    ] {
        let directory = TempDir::new().expect("temp directory");
        let database = directory.path().join(format!("prompt-{failpoint}.db"));
        let params = json!({
            "sessionId": failpoint,
            "idempotencyKey": "prompt",
            "prompt": "prepare approval"
        });
        let mut failing = AppHarness::start_with_prompt_failpoint(&database, true, failpoint).await;
        let failed = failing.request("session.prompt", params.clone()).await;
        assert_eq!(
            failed.response.pointer("/error/data/coolCode"),
            Some(&json!("internal_error"))
        );
        let run_id = failed.events.first().expect("run.started").run_id.clone();
        failing.stop().await;

        let mut recovered = AppHarness::start(&database, true, None).await;
        let retry = recovered.request("session.prompt", params).await;
        assert_eq!(retry.response["result"]["status"], "failed");
        assert!(retry.events.iter().any(|event| {
            event.kind == "worker.failed" && event.payload["code"] == "interrupted_before_receipt"
        }));
        let replay = recovered
            .request("run.events", json!({"runId": run_id, "afterSeq": -1}))
            .await;
        let events: Vec<Event> =
            serde_json::from_value(replay.response["result"]["events"].clone()).unwrap();
        assert!(
            !events
                .iter()
                .any(|event| event.kind == "tool.approval_required")
        );
        let run = recovered.request("run.get", json!({"runId": run_id})).await;
        assert_eq!(run.response["result"]["run"]["toolEffectCount"], 0);
        recovered.stop().await;
    }
}

#[tokio::test]
async fn client_disconnect_does_not_cancel_durable_execution() {
    let directory = TempDir::new().expect("temp directory");
    let database = directory.path().join("disconnect.db");
    let params = json!({
        "sessionId": "disconnect",
        "idempotencyKey": "disconnect-1",
        "prompt": "keep running",
        "mode": "delayed"
    });
    let app = AppHarness::start(&database, true, None).await;
    let AppHarness {
        mut child,
        mut stdin,
        mut lines,
        ..
    } = app;
    let mut request = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "session.prompt",
        "params": params.clone()
    }))
    .unwrap();
    request.push(b'\n');
    stdin.write_all(&request).await.unwrap();
    stdin.flush().await.unwrap();
    loop {
        let line = lines.next_line().await.unwrap().expect("streamed event");
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["params"]["kind"] == "content.delta" {
            break;
        }
    }
    drop(lines);
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("server exits after finishing disconnected request")
        .expect("wait for disconnected server");

    let mut restarted = AppHarness::start(&database, true, None).await;
    let duplicate = restarted.request("session.prompt", params).await;
    assert_eq!(duplicate.response["result"]["status"], "awaiting_approval");
    assert!(duplicate.events.is_empty());
    let run_id = duplicate.response["result"]["runId"].clone();
    let run = restarted.request("run.get", json!({"runId": run_id})).await;
    assert_eq!(run.response["result"]["run"]["workerAttempts"], 1);
    let approved = restarted
        .request(
            "approval.resolve",
            json!({
                "approvalId": duplicate.response["result"]["approvalId"],
                "expectedRevision": 1,
                "approved": true,
                "idempotencyKey": "disconnect-approval"
            }),
        )
        .await;
    assert_eq!(approved.response["result"]["status"], "completed");
    restarted.stop().await;
}

#[tokio::test]
async fn blocked_writer_is_timed_out_without_blocking_durable_execution() {
    let directory = TempDir::new().expect("temp directory");
    let database = directory.path().join("blocked-writer.db");
    let params = json!({
        "sessionId": "blocked-writer",
        "idempotencyKey": "blocked-writer-1",
        "prompt": "bounded delivery",
        "mode": "message-flood"
    });
    let mut input = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session.prompt",
        "params": params.clone()
    }))
    .unwrap();
    input.push(b'\n');
    let store = Store::create(&database).unwrap();
    let core = SpikeCore::new(
        store,
        env!("CARGO_BIN_EXE_cool-m0-spike"),
        CorePolicy {
            allow_write: true,
            ..CorePolicy::default()
        },
    );
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        serve_jsonl(BufReader::new(input.as_slice()), BlockingWriter, core),
    )
    .await
    .expect("delivery deadline prevents a hang");
    assert!(result.is_err());

    let mut restarted = AppHarness::start(&database, true, None).await;
    let duplicate = restarted.request("session.prompt", params).await;
    assert_eq!(duplicate.response["result"]["status"], "failed");
    assert!(duplicate.events.is_empty());
    let run_id = duplicate.response["result"]["runId"].clone();
    let run = restarted
        .request("run.get", json!({"runId": run_id.clone()}))
        .await;
    assert_eq!(run.response["result"]["run"]["workerAttempts"], 1);
    let replay = restarted
        .request("run.catchup", json!({"runId": run_id, "afterSeq": -1}))
        .await;
    assert!(replay.events.iter().any(|event| event.kind == "run.failed"));
    restarted.stop().await;
}

#[tokio::test]
async fn worker_events_stream_before_delayed_worker_finishes() {
    let directory = TempDir::new().expect("temp directory");
    let database = directory.path().join("stream.db");
    let mut app = AppHarness::start(&database, true, None).await;
    let reply = app
        .request(
            "session.prompt",
            json!({
                "sessionId": "stream",
                "idempotencyKey": "stream-1",
                "prompt": "stream",
                "mode": "delayed"
            }),
        )
        .await;
    assert!(reply.first_event_elapsed.is_some());
    let content_delta_elapsed = reply.content_delta_elapsed.expect("streamed content delta");
    assert!(
        reply.total_elapsed.saturating_sub(content_delta_elapsed) >= Duration::from_millis(400)
    );
    assert_eq!(reply.response["result"]["status"], "awaiting_approval");
    app.stop().await;
}

#[tokio::test]
async fn worker_crash_and_output_limits_are_terminal() {
    for mode in ["crash", "oversized", "message-flood"] {
        let directory = TempDir::new().expect("temp directory");
        let database = directory.path().join(format!("{mode}.db"));
        let mut app = AppHarness::start(&database, true, None).await;
        let failed = app
            .request(
                "session.prompt",
                json!({
                    "sessionId": mode,
                    "idempotencyKey": mode,
                    "prompt": mode,
                    "mode": mode
                }),
            )
            .await;
        assert_eq!(failed.response["result"]["status"], "failed");
        assert!(
            failed
                .events
                .iter()
                .any(|event| event.kind == "worker.failed")
        );
        let run_id = failed.response["result"]["runId"].clone();
        app.stop().await;

        let mut restarted = AppHarness::start(&database, true, None).await;
        let replay = restarted
            .request("run.events", json!({"runId": run_id, "afterSeq": -1}))
            .await;
        let events: Vec<Event> =
            serde_json::from_value(replay.response["result"]["events"].clone()).unwrap();
        assert_eq!(ClientState::replay(&events).run_status, "failed");
        restarted.stop().await;
    }
}

#[tokio::test]
async fn tool_effect_identity_collision_fails_the_second_run() {
    let directory = TempDir::new().expect("temp directory");
    let database = directory.path().join("collision.db");
    let mut app = AppHarness::start(&database, true, None).await;
    for (index, mode, expected) in [
        (1, "collision-a", "completed"),
        (2, "collision-b", "failed"),
    ] {
        let prompt = app
            .request(
                "session.prompt",
                json!({
                    "sessionId": format!("collision-{index}"),
                    "idempotencyKey": format!("prompt-{index}"),
                    "prompt": mode,
                    "mode": mode
                }),
            )
            .await;
        let approved = app
            .request(
                "approval.resolve",
                json!({
                    "approvalId": prompt.response["result"]["approvalId"],
                    "expectedRevision": 1,
                    "approved": true,
                    "idempotencyKey": format!("approval-{index}")
                }),
            )
            .await;
        assert_eq!(approved.response["result"]["status"], expected);
        if index == 2 {
            assert!(approved.events.iter().any(|event| {
                event.kind == "tool.failed" && event.payload["code"] == "effect_identity_conflict"
            }));
        }
    }
    app.stop().await;
}

#[tokio::test]
async fn foreign_database_and_protocol_contract_are_rejected_safely() {
    let directory = TempDir::new().expect("temp directory");
    let foreign = directory.path().join("foreign.db");
    Connection::open(&foreign)
        .unwrap()
        .execute("CREATE TABLE user_data(value TEXT)", [])
        .unwrap();
    assert!(matches!(
        Store::create(&foreign),
        Err(SpikeError::ForeignDatabase)
    ));

    let database = directory.path().join("protocol.db");
    let mut app = AppHarness::start(&database, false, None).await;
    let unknown = app.request("unknown.method", json!({})).await;
    assert_eq!(unknown.response["error"]["code"], -32601);
    assert_eq!(
        unknown.response["error"]["data"]["coolCode"],
        "method_not_found"
    );

    let oversized = vec![b'x'; 64 * 1024 + 1];
    app.stdin.write_all(&oversized).await.unwrap();
    app.stdin.write_all(b"\n").await.unwrap();
    app.stdin.flush().await.unwrap();
    let response = app.read_reply(Value::Null, Instant::now()).await;
    assert_eq!(
        response.response["error"]["data"]["coolCode"],
        "frame_too_large"
    );
    let initialize = app.request("initialize", json!({})).await;
    assert_eq!(initialize.response["result"]["protocolVersion"], 1);
    app.stop().await;
}
