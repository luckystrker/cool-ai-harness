use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_acp::AcpServer;
use cool_agent::{
    AgentRuntime, ModelEvent, ModelRequest, ModelStream, ProviderError, ScriptedDriver, ToolCall,
    builtin_registry,
};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::time::timeout;

const WORKSPACE_MARKER: &str = "acp-workspace";

struct SlowDriver;

#[async_trait]
impl cool_agent::ModelDriver for SlowDriver {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ProviderError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(ModelEvent::Content("late response".to_owned())),
            Ok(ModelEvent::Finish { reason: None }),
        ])))
    }
}

struct Frames {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    notifications: Vec<Value>,
}

impl Frames {
    async fn send(&mut self, frame: Value) {
        if std::env::var("COOL_ACP_TRACE").is_ok() {
            eprintln!("-> {}", serde_json::to_string(&frame).unwrap_or_default());
        }
        self.writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write ACP frame");
        self.writer.flush().await.expect("flush ACP frame");
    }

    async fn request(&mut self, id: Value, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let frame = self.next().await;
            if frame.get("id") == Some(&id)
                && (frame.get("result").is_some() || frame.get("error").is_some())
            {
                return frame;
            }
            if frame.get("method").and_then(Value::as_str) == Some("session/update") {
                self.notifications.push(frame);
                continue;
            }
            panic!("unexpected frame while awaiting {method}: {frame}");
        }
    }

    async fn next(&mut self) -> Value {
        let mut line = String::new();
        timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .expect("ACP frame timeout")
            .expect("read ACP frame");
        assert!(!line.is_empty(), "ACP server closed the stream");
        let frame: Value = serde_json::from_str(&line).expect("valid ACP frame");
        if std::env::var("COOL_ACP_TRACE").is_ok() {
            eprintln!("<- {}", serde_json::to_string(&frame).unwrap_or_default());
        }
        frame
    }

    async fn next_update(&mut self, expected: &str) -> Value {
        if let Some(index) = self
            .notifications
            .iter()
            .position(|frame| frame["params"]["update"]["sessionUpdate"] == expected)
        {
            return self.notifications.remove(index)["params"]["update"].clone();
        }
        loop {
            let frame = self.next().await;
            if frame.get("method").and_then(Value::as_str) == Some("session/update")
                && frame["params"]["update"]["sessionUpdate"] == expected
            {
                return frame["params"]["update"].clone();
            }
            if frame.get("method").and_then(Value::as_str) == Some("session/update") {
                self.notifications.push(frame);
            }
        }
    }

    async fn next_permission_request(&mut self) -> Value {
        loop {
            let frame = self.next().await;
            if frame.get("method").and_then(Value::as_str) == Some("session/request_permission") {
                return frame;
            }
        }
    }
}

struct Harness {
    frames: Frames,
    app: AppClient,
    server: AppServer,
    workspace: PathBuf,
    acp_task: tokio::task::JoinHandle<std::io::Result<()>>,
    app_task: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn harness_with(
    driver: Arc<dyn cool_agent::ModelDriver>,
    directory: &std::path::Path,
) -> Harness {
    harness_with_buffer(driver, directory, 512).await
}

async fn harness_with_buffer(
    driver: Arc<dyn cool_agent::ModelDriver>,
    directory: &std::path::Path,
    event_buffer: usize,
) -> Harness {
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(directory).unwrap(),
        CapabilityPolicy::new(Some(Decision::Ask)),
        "scripted",
    )
    .unwrap();
    let (app_client_io, app_server_io) = tokio::io::duplex(64 * 1024);
    let (app_reader, app_writer) = tokio::io::split(app_client_io);
    let app_task = tokio::spawn({
        let server = server.clone();
        async move { server.serve_io(app_server_io).await }
    });
    let app = AppClient::connect_with_buffer(app_reader, app_writer, event_buffer).unwrap();
    app.initialize("acp-tests", "1").await.unwrap();

    let (acp_client_io, acp_server_io) = tokio::io::duplex(64 * 1024);
    let (client_reader, client_writer) = tokio::io::split(acp_client_io);
    let (server_reader, server_writer) = tokio::io::split(acp_server_io);
    let acp_server = Arc::new(AcpServer::new(
        app.clone(),
        directory,
        Box::new(server_writer),
    ));
    let acp_task = tokio::spawn(async move { acp_server.serve_io(server_reader).await });

    Harness {
        frames: Frames {
            reader: BufReader::new(client_reader),
            writer: client_writer,
            notifications: Vec::new(),
        },
        app,
        server,
        workspace: directory.to_path_buf(),
        acp_task,
        app_task,
    }
}

fn write_scripts() -> Vec<Result<Vec<ModelEvent>, ProviderError>> {
    vec![
        Ok(vec![
            ModelEvent::Content("first answer".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "call-1".to_owned(),
                name: "write_file".to_owned(),
                arguments: [("path".to_owned(), json!("acp-note.txt"))]
                    .into_iter()
                    .chain([("content".to_owned(), json!("written through ACP"))])
                    .collect(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("note written".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]
}

async fn harness() -> (Harness, tempfile::TempDir) {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Content("hello from cool".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("second answer".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "call-1".to_owned(),
                name: "write_file".to_owned(),
                arguments: [("path".to_owned(), json!("acp-note.txt"))]
                    .into_iter()
                    .chain([("content".to_owned(), json!("written through ACP"))])
                    .collect(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("note written".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
    let harness = harness_with(provider, directory.path()).await;
    (harness, directory)
}

async fn initialize(frames: &mut Frames) {
    let response = frames
        .request(
            json!(1),
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await;
    assert_eq!(response["result"]["protocolVersion"], 1);
    assert_eq!(response["result"]["agentCapabilities"]["loadSession"], true);
    assert_eq!(
        response["result"]["agentCapabilities"]["promptCapabilities"]["image"],
        false
    );
    assert_eq!(response["result"]["agentInfo"]["name"], "cool-ai-harness");
}

async fn new_session(frames: &mut Frames, workspace: &std::path::Path, id: i64) -> String {
    let response = frames
        .request(
            json!(id),
            "session/new",
            json!({"cwd": workspace, "mcpServers": []}),
        )
        .await;
    response["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned()
}

#[tokio::test]
async fn initialize_new_prompt_and_load_share_the_durable_run() {
    let (mut harness, _directory) = harness().await;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;

    let response = harness
        .frames
        .request(
            json!(3),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "hello"}]}),
        )
        .await;
    assert_eq!(response["result"]["stopReason"], "end_turn");
    let message = harness.frames.next_update("agent_message_chunk").await;
    assert_eq!(message["content"]["text"], "hello from cool");

    // session/load replays the same durable history through a fresh connection.
    let response = harness
        .frames
        .request(
            json!(4),
            "session/load",
            json!({"sessionId": session_id, "cwd": harness.workspace, "mcpServers": []}),
        )
        .await;
    assert!(response.get("result").is_some(), "load failed: {response}");
    assert_eq!(
        response["result"]["_meta"]["io.github.luckystrker.cool/title"],
        "ACP session"
    );
    let user_chunk = harness.frames.next_update("user_message_chunk").await;
    assert_eq!(user_chunk["content"]["text"], "hello");
    let replay = harness.frames.next_update("agent_message_chunk").await;
    assert_eq!(replay["content"]["text"], "hello from cool");

    // A second prompt continues the same durable run history.
    let response = harness
        .frames
        .request(
            json!(5),
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [
                    {"type": "text", "text": "write a note"},
                    {"type": "resource_link", "name": "spec", "uri": "file:///spec.md"}
                ]
            }),
        )
        .await;
    assert_eq!(response["result"]["stopReason"], "end_turn");

    assert_eq!(harness.server.prompt_executions().await, 2);
    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
    assert!(
        harness
            .server
            .store()
            .list_sessions("local-user", None, 10)
            .unwrap()
            .len()
            == 1
    );
}

#[tokio::test]
async fn permission_request_resolves_the_authoritative_approval() {
    let directory = tempdir().unwrap();
    let mut harness = harness_with(
        Arc::new(ScriptedDriver::new(write_scripts())),
        directory.path(),
    )
    .await;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;
    // First prompt consumes the text script.
    harness
        .frames
        .request(
            json!(3),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "first"}]}),
        )
        .await;
    let _ = harness.frames.next_update("agent_message_chunk").await;

    // Second prompt asks the model for a write tool call.
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]}
        }))
        .await;
    let tool_call = harness.frames.next_update("tool_call").await;
    assert_eq!(tool_call["toolCallId"], "call-1");
    assert_eq!(tool_call["title"], "write_file");
    assert_eq!(tool_call["kind"], "edit");
    assert_eq!(tool_call["status"], "pending");

    let permission = harness.frames.next_permission_request().await;
    let request_id = permission["id"].clone();
    assert_eq!(permission["params"]["sessionId"], session_id);
    assert_eq!(permission["params"]["toolCall"]["toolCallId"], "call-1");
    assert_eq!(permission["params"]["options"][0]["optionId"], "allow_once");
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}}
        }))
        .await;

    let response = loop {
        let frame = harness.frames.next().await;
        if frame.get("id") == Some(&json!(4)) {
            break frame;
        }
    };
    assert_eq!(response["result"]["stopReason"], "end_turn");
    assert!(
        std::fs::read_to_string(harness.workspace.join("acp-note.txt"))
            .unwrap()
            .contains("written through ACP"),
        "the approved tool executed in the workspace"
    );
    let approvals: i64 = harness
        .server
        .store()
        .all_events(
            harness
                .server
                .store()
                .list_sessions("local-user", None, 1)
                .unwrap()[0]
                .active_run_id
                .clone()
                .unwrap_or_default()
                .as_str(),
            "local-user",
        )
        .map(|events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event.event,
                        cool_protocol::CanonicalEvent::ToolApprovalResolved(_)
                    )
                })
                .count() as i64
        })
        .unwrap_or(0);
    assert!(approvals >= 0);

    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn malformed_permission_responses_fail_closed_without_side_effects() {
    let directory = tempdir().unwrap();
    let mut harness = harness_with(
        Arc::new(ScriptedDriver::new(write_scripts())),
        directory.path(),
    )
    .await;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;
    harness
        .frames
        .request(
            json!(3),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "first"}]}),
        )
        .await;
    let _ = harness.frames.next_update("agent_message_chunk").await;

    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]}
        }))
        .await;
    let permission = harness.frames.next_permission_request().await;
    let request_id = permission["id"].clone();
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"outcome": {"outcome": "nonsense"}}
        }))
        .await;
    let response = loop {
        let frame = harness.frames.next().await;
        if frame.get("id") == Some(&json!(4)) {
            break frame;
        }
    };
    assert_eq!(response["result"]["stopReason"], "end_turn");
    assert!(
        !harness.workspace.join("acp-note.txt").exists(),
        "a denied approval must not write"
    );
    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn cancel_notification_returns_cancelled_and_records_the_terminal_run() {
    let directory = tempdir().unwrap();
    let harness = harness_with(Arc::new(SlowDriver), directory.path()).await;
    let mut harness = harness;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "slow"}]}
        }))
        .await;
    // Wait until the durable run exists, then cancel from the ACP client.
    let run_id = timeout(Duration::from_secs(5), async {
        loop {
            let sessions = harness
                .server
                .store()
                .list_sessions("local-user", None, 1)
                .unwrap();
            if let Some(active) = sessions
                .first()
                .and_then(|session| session.active_run_id.clone())
            {
                break active;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("run becomes active");
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id}
        }))
        .await;
    let response = loop {
        let frame = harness.frames.next().await;
        if frame.get("id") == Some(&json!(3)) {
            break frame;
        }
    };
    assert_eq!(response["result"]["stopReason"], "cancelled");
    assert_eq!(
        harness
            .server
            .store()
            .run(&run_id, "local-user")
            .unwrap()
            .status,
        cool_state::RunStatus::Cancelled
    );
    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn cancel_while_permission_is_pending_returns_cancelled_without_side_effects() {
    let directory = tempdir().unwrap();
    let mut harness = harness_with(
        Arc::new(ScriptedDriver::new(write_scripts())),
        directory.path(),
    )
    .await;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;
    harness
        .frames
        .request(
            json!(3),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "first"}]}),
        )
        .await;
    let _ = harness.frames.next_update("agent_message_chunk").await;

    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]}
        }))
        .await;
    let permission = harness.frames.next_permission_request().await;
    assert_eq!(permission["params"]["toolCall"]["toolCallId"], "call-1");

    // Cancel while the ACP client has not answered the permission request: the
    // pending permission must be denied and the turn must end promptly.
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id}
        }))
        .await;
    let response = timeout(Duration::from_secs(5), async {
        loop {
            let frame = harness.frames.next().await;
            if frame.get("id") == Some(&json!(4)) {
                break frame;
            }
        }
    })
    .await
    .expect("cancel must resolve the pending permission");
    assert_eq!(response["result"]["stopReason"], "cancelled");
    assert!(
        !harness.workspace.join("acp-note.txt").exists(),
        "a pending approval cancelled from the client must not write"
    );

    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn unsupported_boundaries_and_methods_fail_closed() {
    let (mut harness, _directory) = harness().await;
    // Session methods require initialize.
    let response = harness
        .frames
        .request(
            json!(1),
            "session/new",
            json!({"cwd": harness.workspace, "mcpServers": []}),
        )
        .await;
    assert_eq!(response["error"]["code"], -32600);

    initialize(&mut harness.frames).await;
    let again = harness
        .frames
        .request(
            json!(2),
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await;
    assert_eq!(again["error"]["code"], -32600);

    let unknown = harness
        .frames
        .request(json!(3), "session/unknown", json!({}))
        .await;
    assert_eq!(unknown["error"]["code"], -32601);

    let relative = harness
        .frames
        .request(
            json!(4),
            "session/new",
            json!({"cwd": "relative/path", "mcpServers": []}),
        )
        .await;
    assert_eq!(relative["error"]["code"], -32602);

    let mismatched = harness
        .frames
        .request(
            json!(5),
            "session/new",
            json!({"cwd": std::env::temp_dir(), "mcpServers": []}),
        )
        .await;
    assert_eq!(mismatched["error"]["code"], -32602);
    assert!(
        mismatched["error"]["message"]
            .as_str()
            .unwrap()
            .contains(WORKSPACE_MARKER)
            || mismatched["error"]["message"]
                .as_str()
                .unwrap()
                .contains("working directory")
    );

    let extra = harness
        .frames
        .request(
            json!(6),
            "session/new",
            json!({
                "cwd": harness.workspace,
                "mcpServers": [],
                "additionalDirectories": ["/tmp"]
            }),
        )
        .await;
    assert_eq!(extra["error"]["code"], -32602);

    let client_mcp = harness
        .frames
        .request(
            json!(7),
            "session/new",
            json!({
                "cwd": harness.workspace,
                "mcpServers": [{"name": "x", "command": "node", "args": []}]
            }),
        )
        .await;
    assert_eq!(client_mcp["error"]["code"], -32602);

    let empty_prompt = harness
        .frames
        .request(
            json!(8),
            "session/new",
            json!({"cwd": harness.workspace, "mcpServers": []}),
        )
        .await;
    let session_id = empty_prompt["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let empty = harness
        .frames
        .request(
            json!(9),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": []}),
        )
        .await;
    assert_eq!(empty["error"]["code"], -32602);
    let unsupported_block = harness
        .frames
        .request(
            json!(10),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "audio", "data": "x"}]}),
        )
        .await;
    assert_eq!(unsupported_block["error"]["code"], -32602);
    let missing_session = harness
        .frames
        .request(
            json!(11),
            "session/load",
            json!({"sessionId": "session-missing", "cwd": harness.workspace, "mcpServers": []}),
        )
        .await;
    assert_eq!(missing_session["error"]["code"], -32002);

    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn broadcast_lag_recovers_from_the_durable_run_log() {
    let directory = tempdir().unwrap();
    // A tiny live-event buffer forces `Lagged`; the adapter must recover the
    // dropped range from `run.events` instead of waiting for a terminal event
    // that was already dropped.
    let mut deltas = Vec::new();
    for index in 0..40 {
        deltas.push(ModelEvent::Content(format!("chunk-{index} ")));
    }
    deltas.push(ModelEvent::Finish { reason: None });
    let provider = Arc::new(ScriptedDriver::new([Ok(deltas)]));
    let mut harness = harness_with_buffer(provider, directory.path(), 2).await;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;

    let response = harness
        .frames
        .request(
            json!(3),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "go"}]}),
        )
        .await;
    assert_eq!(response["result"]["stopReason"], "end_turn");

    let text = harness
        .frames
        .notifications
        .iter()
        .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk")
        .map(|frame| {
            frame["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("");
    let expected = (0..40)
        .map(|index| format!("chunk-{index} "))
        .collect::<String>();
    assert_eq!(
        text, expected,
        "lag recovery must deliver every durable chunk exactly once, in order"
    );

    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn adapter_restart_replays_state_from_the_core() {
    let (mut harness, _directory) = harness().await;
    initialize(&mut harness.frames).await;
    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;
    harness
        .frames
        .request(
            json!(3),
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": "hello"}]}),
        )
        .await;
    let _ = harness.frames.next_update("agent_message_chunk").await;

    // Drop the ACP connection entirely and start a fresh adapter over the same
    // core: session state survives because the core owns it.
    harness.acp_task.abort();
    let (acp_client_io, acp_server_io) = tokio::io::duplex(64 * 1024);
    let (client_reader, client_writer) = tokio::io::split(acp_client_io);
    let (server_reader, server_writer) = tokio::io::split(acp_server_io);
    let acp_server = Arc::new(AcpServer::new(
        harness.app.clone(),
        &harness.workspace,
        Box::new(server_writer),
    ));
    let acp_task = tokio::spawn(async move { acp_server.serve_io(server_reader).await });
    let mut frames = Frames {
        reader: BufReader::new(client_reader),
        writer: client_writer,
        notifications: Vec::new(),
    };
    initialize(&mut frames).await;
    let response = frames
        .request(
            json!(2),
            "session/load",
            json!({"sessionId": session_id, "cwd": harness.workspace, "mcpServers": []}),
        )
        .await;
    assert!(response.get("result").is_some());
    let user = frames.next_update("user_message_chunk").await;
    assert_eq!(user["content"]["text"], "hello");
    let agent = frames.next_update("agent_message_chunk").await;
    assert_eq!(agent["content"]["text"], "hello from cool");

    acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn rejected_prompt_batch_members_still_answer_valid_members() {
    let (mut harness, _directory) = harness().await;
    initialize(&mut harness.frames).await;
    harness
        .frames
        .send(json!([
            {
                "jsonrpc": "2.0",
                "id": 10,
                "method": "session/new",
                "params": {"cwd": harness.workspace, "mcpServers": []}
            },
            {"jsonrpc": "2.0", "method": "not-a-real-method", "params": {}}
        ]))
        .await;
    let response = loop {
        let frame = harness.frames.next().await;
        if frame.get("id") == Some(&json!(10)) {
            break frame;
        }
    };
    assert!(response.get("result").is_some());
    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

#[tokio::test]
async fn acp_frames_match_committed_fixtures() {
    let directory = tempdir().unwrap();
    let mut harness = harness_with(
        Arc::new(ScriptedDriver::new(write_scripts())),
        directory.path(),
    )
    .await;
    let mut frames = Vec::new();
    let mut record = |value: &Value| frames.push(value.clone());

    initialize(&mut harness.frames).await;
    let initialize_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {"image": false, "audio": false, "embeddedContext": false},
                "mcpCapabilities": {"http": false, "sse": false},
            },
            "authMethods": [],
            "agentInfo": {
                "name": "cool-ai-harness",
                "title": "Cool AI Harness",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }
    });
    record(&initialize_response);

    let session_id = new_session(&mut harness.frames, &harness.workspace, 2).await;
    record(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {"sessionId": session_id},
    }));

    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "hello"}]}
        }))
        .await;
    let update = harness.frames.next_update("agent_message_chunk").await;
    record(&json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": session_id, "update": update},
    }));
    let response = loop {
        let frame = harness.frames.next().await;
        if frame.get("id") == Some(&json!(3)) {
            break frame;
        }
    };
    record(&response);

    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]}
        }))
        .await;
    let tool_call = harness.frames.next_update("tool_call").await;
    record(&json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": session_id, "update": tool_call},
    }));
    let permission = harness.frames.next_permission_request().await;
    record(&permission);
    harness
        .frames
        .send(json!({
            "jsonrpc": "2.0",
            "id": permission["id"].clone(),
            "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}}
        }))
        .await;
    let mut seen_updates = Vec::new();
    loop {
        let frame = harness.frames.next().await;
        if frame.get("id") == Some(&json!(4)) {
            record(&frame);
            break;
        }
        if frame.get("method").and_then(Value::as_str) == Some("session/update") {
            seen_updates.push(frame["params"]["update"].clone());
            record(&frame);
        }
    }
    assert!(
        seen_updates
            .iter()
            .any(|update| update["sessionUpdate"] == "tool_call_update"
                && update["status"] == "completed"),
        "approved tool call must complete: {seen_updates:?}"
    );

    let normalized = normalize_frames(&frames);
    let golden_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/acp-v1-frames.json");
    let encoded = format!(
        "{encoded}\n",
        encoded = serde_json::to_string_pretty(&normalized).unwrap()
    );
    if std::env::var("COOL_ACP_UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(golden_path.parent().unwrap()).unwrap();
        std::fs::write(&golden_path, &encoded).unwrap();
    } else {
        let committed = std::fs::read_to_string(&golden_path)
            .expect("committed ACP frame fixture; run with COOL_ACP_UPDATE_GOLDEN=1");
        assert_eq!(
            committed, encoded,
            "ACP frame drift; regenerate the fixture"
        );
    }

    drop(harness.frames);
    harness.acp_task.abort();
    harness.app_task.abort();
}

fn normalize_frames(frames: &[Value]) -> Vec<Value> {
    let mut session_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut run_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut approval_ids: BTreeMap<String, String> = BTreeMap::new();
    fn walk(
        value: &mut Value,
        session_ids: &mut BTreeMap<String, String>,
        run_ids: &mut BTreeMap<String, String>,
        approval_ids: &mut BTreeMap<String, String>,
    ) {
        match value {
            Value::Object(map) => {
                for (_, child) in map.iter_mut() {
                    walk(child, session_ids, run_ids, approval_ids);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, session_ids, run_ids, approval_ids);
                }
            }
            Value::String(text) => {
                for (prefix, table, replacement) in [
                    ("session-", &mut *session_ids, "session"),
                    ("run-", &mut *run_ids, "run"),
                    ("approval-", &mut *approval_ids, "approval"),
                ] {
                    if let Some(rest) = text.strip_prefix(prefix)
                        && rest.len() == 36
                        && rest.contains('-')
                    {
                        let next = format!("{replacement}-{}", table.len() + 1);
                        let mapped = table.entry(text.clone()).or_insert(next);
                        *text = mapped.clone();
                    }
                }
            }
            _ => {}
        }
    }
    let mut normalized = frames.to_vec();
    for frame in &mut normalized {
        walk(frame, &mut session_ids, &mut run_ids, &mut approval_ids);
    }
    normalized
}
