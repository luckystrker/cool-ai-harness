use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{
    AgentRuntime, ModelEvent, ScriptedDriver, Tool, ToolCall, ToolContext, ToolDefinition,
    ToolError, ToolHandler, ToolResult, builtin_registry,
};
use cool_app_server::client::new_idempotency_key;
use cool_app_server::{AppClient, AppServer, RunLifecycle, ServerConfig};
use cool_protocol::{CanonicalEvent, StatusEntry, StatusGetResult};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

struct SlowTool;

#[async_trait]
impl ToolHandler for SlowTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(ToolResult::ok(json!("slow result")))
    }
}

async fn connected_client(
    server: AppServer,
) -> (AppClient, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client.initialize("session-tests", "1").await.unwrap();
    (client, task)
}

fn scripted_server(driver: Arc<ScriptedDriver>, workspace: &std::path::Path) -> AppServer {
    AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(workspace).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap()
}

async fn drain_run(
    mut events: tokio::sync::broadcast::Receiver<cool_protocol::EventEnvelope>,
    run_id: &str,
) -> Vec<CanonicalEvent> {
    let mut collected = Vec::new();
    loop {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event arrival timeout")
            .expect("event channel open");
        if envelope.run_id != run_id {
            continue;
        }
        let terminal = matches!(
            envelope.event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        );
        collected.push(envelope.event);
        if terminal {
            return collected;
        }
    }
}

#[tokio::test]
async fn session_list_history_and_fork_are_protocol_visible() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Content("first".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("second".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server.clone()).await;

    let session_id = client
        .create_session("list-key", Some("listed"), Some("project-a"))
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt("list-prompt", &session_id, "question", None)
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;

    let listed = client.list_sessions(Some("project-a"), 10).await.unwrap();
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].session_id, session_id);
    assert_eq!(listed.sessions[0].title.as_deref(), Some("listed"));
    assert_eq!(listed.sessions[0].project_key.as_deref(), Some("project-a"));
    assert!(
        client
            .list_sessions(Some("project-b"), 10)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    assert!(matches!(
        client.list_sessions(None, 0).await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "invalid_session_list_limit"
    ));

    let history = client.session_history(&session_id, 50).await.unwrap();
    assert!(!history.has_more);
    let roles = history
        .items
        .iter()
        .map(|item| item.role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(roles, ["user", "assistant"]);
    assert_eq!(history.items[0].content.as_deref(), Some("question"));
    assert_eq!(history.items[1].content.as_deref(), Some("first"));

    let forked = client
        .fork_session("fork-key", &session_id, Some("branch"))
        .await
        .unwrap();
    assert_eq!(forked.forked_from, session_id);
    assert_ne!(forked.session_id, session_id);
    let forked_history = client
        .session_history(&forked.session_id, 50)
        .await
        .unwrap();
    assert_eq!(
        forked_history
            .items
            .iter()
            .map(|item| item.content.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["question", "first"]
    );
    let replay = client
        .fork_session("fork-key", &session_id, Some("branch"))
        .await
        .unwrap();
    assert_eq!(replay.session_id, forked.session_id);

    let forked_events = client.subscribe();
    let forked_run = client
        .prompt("fork-prompt", &forked.session_id, "follow-up", None)
        .await
        .unwrap()
        .run_id;
    let forked_events = drain_run(forked_events, &forked_run).await;
    assert!(matches!(
        forked_events.last().unwrap(),
        CanonicalEvent::RunCompleted(_)
    ));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn session_steer_is_durable_and_reaches_the_next_model_request() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "call-1".to_owned(),
                name: "slow_tool".to_owned(),
                arguments: Default::default(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("finished".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
    let mut registry = builtin_registry();
    registry = registry
        .extend([Tool::new(
            ToolDefinition {
                name: "slow_tool".to_owned(),
                description: "slow deterministic fixture".to_owned(),
                parameters: json!({"type": "object"}),
            },
            [],
            Decision::Allow,
            SlowTool,
        )])
        .unwrap();
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider.clone(), registry),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;
    let session_id = client
        .create_session("steer-session", None, None)
        .await
        .unwrap();
    let mut events = client.subscribe();
    let run_id = client
        .prompt("steer-prompt", &session_id, "start", None)
        .await
        .unwrap()
        .run_id;

    // Steer while the slow tool keeps the run active so the message is drained
    // at the next iteration boundary.
    let steer = client
        .steer("steer-key", &run_id, "use the other file")
        .await
        .unwrap();
    assert_eq!(steer.run_id, run_id);
    assert!(steer.seq >= 3);
    let steer_seq = steer.seq;

    let replay = client
        .steer("steer-key", &run_id, "use the other file")
        .await
        .unwrap();
    assert_eq!(replay.seq, steer_seq);
    assert!(matches!(
        client.steer("steer-key", &run_id, "different").await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "idempotency_conflict"
    ));
    assert!(matches!(
        client
            .steer(&new_idempotency_key("empty"), &run_id, "   ")
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "empty_steer_content"
    ));

    let mut terminal = false;
    while !terminal {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event arrival timeout")
            .expect("event channel open");
        if envelope.run_id != run_id {
            continue;
        }
        terminal = matches!(
            envelope.event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        );
    }
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2);
    let second = requests[1]
        .messages
        .iter()
        .filter_map(|message| message.content.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        second
            .iter()
            .filter(|content| content.as_str() == "use the other file")
            .count(),
        1,
        "steer is folded into the model history exactly once"
    );

    let events = server.events_for_run(&run_id).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.event,
        CanonicalEvent::ItemCompleted(item)
            if item.content.as_deref() == Some("use the other file")
    )));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn steer_is_rejected_for_terminal_and_foreign_runs() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("done".to_owned()),
        ModelEvent::Finish { reason: None },
    ])]));
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server.clone()).await;
    let session_id = client
        .create_session("terminal-session", None, None)
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt("terminal-prompt", &session_id, "hello", None)
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;
    assert!(matches!(
        client
            .steer(&new_idempotency_key("late"), &run_id, "too late")
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "run_not_active"
    ));
    assert!(matches!(
        client
            .steer(&new_idempotency_key("missing"), "run-missing", "hello")
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "run_not_found"
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

struct StaticStatus;

#[async_trait]
impl RunLifecycle for StaticStatus {
    async fn on_event(
        &self,
        _event: &str,
        _payload: serde_json::Value,
        _policy: &CapabilityPolicy,
    ) -> Vec<CanonicalEvent> {
        Vec::new()
    }

    async fn status(&self) -> Option<StatusGetResult> {
        Some(StatusGetResult {
            plugins: vec![StatusEntry {
                id: "demo".to_owned(),
                status: "enabled".to_owned(),
                code: None,
            }],
            workers: vec![StatusEntry {
                id: "worker-1".to_owned(),
                status: "running".to_owned(),
                code: None,
            }],
            mcp_servers: vec!["demo/files".to_owned()],
        })
    }
}

#[tokio::test]
async fn status_get_reports_the_configured_lifecycle_snapshot() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;
    let empty = client.status().await.unwrap();
    assert!(empty.plugins.is_empty());
    assert!(empty.workers.is_empty());
    assert!(empty.mcp_servers.is_empty());
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    let server = server.with_run_lifecycle(Arc::new(StaticStatus));
    let (client, task) = connected_client(server).await;
    let status = client.status().await.unwrap();
    assert_eq!(status.plugins[0].id, "demo");
    assert_eq!(status.plugins[0].status, "enabled");
    assert_eq!(status.workers[0].status, "running");
    assert_eq!(status.mcp_servers, ["demo/files"]);
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn reconnect_catch_up_has_no_gaps_duplicates_or_repeated_side_effects() {
    let directory = tempdir().unwrap();
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let provider = Arc::new(ScriptedDriver::echo_with_delay(config.event_delay));
    let server = AppServer::with_agent_runtime(
        config,
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;
    let session_id = client
        .create_session("reconnect-session", None, None)
        .await
        .unwrap();
    let mut events = client.subscribe();
    let run_id = client
        .prompt("reconnect-prompt", &session_id, "hello", None)
        .await
        .unwrap()
        .run_id;

    // Read the first two events, then drop the client mid-run. The server
    // cancels the owned run on disconnect.
    let mut seen = Vec::new();
    for _ in 0..2 {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        if envelope.run_id == run_id {
            seen.push(envelope);
        }
    }
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
    timeout(Duration::from_secs(5), async {
        loop {
            if server
                .store()
                .run(&run_id, "local-user")
                .is_ok_and(|run| run.status == cool_state::RunStatus::Cancelled)
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("disconnect cancellation becomes terminal");

    let (client, task) = connected_client(server.clone()).await;
    let retried = client
        .prompt("reconnect-prompt", &session_id, "hello", None)
        .await
        .unwrap();
    assert_eq!(retried.run_id, run_id, "idempotent retry reuses the run");
    assert_eq!(server.prompt_executions().await, 1);

    let mut pages = Vec::new();
    let mut after = None;
    loop {
        let page = client.run_events(&run_id, after, 2).await.unwrap();
        after = page
            .next_cursor
            .as_ref()
            .and_then(|cursor| cursor.after_seq);
        let has_more = page.has_more;
        pages.extend(page.events);
        if !has_more {
            break;
        }
    }
    let sequences = pages.iter().map(|event| event.seq).collect::<Vec<_>>();
    assert_eq!(sequences, (1..=sequences.len() as u64).collect::<Vec<_>>());
    let unique = pages
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), pages.len());
    let caught_up = seen
        .iter()
        .all(|event| pages.iter().any(|page| page.event_id == event.event_id));
    assert!(caught_up, "every observed event is in the durable log");
    assert!(matches!(
        pages.last().unwrap().event,
        CanonicalEvent::RunCancelled(_)
    ));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn oversized_history_is_bounded_by_the_negotiated_frame_limit() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = AppServer::with_agent_runtime(
        ServerConfig {
            max_frame_bytes: 1_200,
            ..ServerConfig::default()
        },
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;

    // The transport frame guard rejects oversized live events, so a store-level
    // append proves the read path also stays bounded.
    let append = |session: &str, run: &str, content: &str| {
        let envelope = cool_protocol::EventEnvelope {
            event_id: format!("event-{run}-{}", content.len()),
            schema_version: cool_protocol::V1Version::VALUE,
            session_id: session.to_owned(),
            run_id: run.to_owned(),
            item_id: None,
            seq: 0,
            occurred_at: "2026-09-17T00:00:00.000Z".to_owned(),
            actor: cool_protocol::ActorRef {
                id: "local-user".to_owned(),
                kind: cool_protocol::ActorKind::LocalUser,
            },
            source: "test".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::ItemCompleted(cool_protocol::ItemEvent {
                role: Some("user".to_owned()),
                content: Some(content.to_owned()),
                tool_calls: Vec::new(),
            }),
            extensions: Default::default(),
        };
        server
            .store()
            .append_event_auto("local-user", envelope)
            .unwrap();
    };

    let huge = server
        .store()
        .create_session("local-user", "huge-session", "huge-session", None, None)
        .unwrap()
        .value;
    let huge_run = server
        .store()
        .start_run("local-user", "huge-run", "huge-run", &huge)
        .unwrap()
        .value;
    append(&huge, &huge_run, &"x".repeat(4_000));
    assert!(matches!(
        client.session_history(&huge, 50).await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "outbound_frame_too_large"
    ));

    let mixed = server
        .store()
        .create_session("local-user", "mixed-session", "mixed-session", None, None)
        .unwrap()
        .value;
    let mixed_run = server
        .store()
        .start_run("local-user", "mixed-run", "mixed-run", &mixed)
        .unwrap()
        .value;
    append(&mixed, &mixed_run, &"x".repeat(4_000));
    append(&mixed, &mixed_run, "short history tail");
    let page = client.session_history(&mixed, 50).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].content.as_deref(), Some("short history tail"));
    assert!(page.has_more, "trimmed items are reported as older history");

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn oversized_session_labels_are_rejected_before_persistence() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server.clone()).await;
    let long = "x".repeat(201);
    assert!(matches!(
        client
            .create_session("label-session", Some(&long), None)
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "label_too_long"
    ));
    assert!(matches!(
        client
            .create_session("label-session", None, Some(&long))
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "label_too_long"
    ));
    let session_id = client
        .create_session("label-session", Some(&"x".repeat(200)), None)
        .await
        .unwrap();
    assert!(matches!(
        client
            .fork_session("label-fork", &session_id, Some(&long))
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "label_too_long"
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn client_requests_fail_closed_before_initialize() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider, directory.path());
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).unwrap();
    let result = client.list_sessions(None, 10).await;
    assert!(matches!(
        result,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "not_initialized"
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}
