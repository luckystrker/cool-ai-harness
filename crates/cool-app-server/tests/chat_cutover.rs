//! M11 chat-cutover foundations: legacy conversation -> durable session link,
//! canonical transcript import, and bounded reads for the React surface.
//!
//! These tests drive the real JSON-RPC client against an `AppServer` backed by
//! an in-memory legacy store plus the in-memory durable store, so the exercised
//! path is exactly the one `cool serve` uses.

use std::sync::Arc;
use std::time::Duration;

use cool_agent::{AgentRuntime, ModelEvent, ScriptedDriver, ToolCall, builtin_registry};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::*;
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::domains::conversations::{NewConversation, NewMessage};
use cool_store::domains::runs::RunFilter;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::timeout;

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("valid key")
}

fn legacy_server() -> (AppServer, Arc<LegacyStore>) {
    let store = LegacyStore::in_memory().expect("in-memory legacy store");
    store.ensure_actor("local-user").expect("actor");
    let store = Arc::new(store);
    let config = ServerConfig {
        legacy_store: Some(store.clone()),
        ..ServerConfig::default()
    };
    let server =
        AppServer::with_store(config, DurableStore::in_memory().expect("durable")).expect("server");
    (server, store)
}

fn scripted_legacy_server(
    provider: Arc<ScriptedDriver>,
    workspace: &std::path::Path,
) -> (AppServer, Arc<LegacyStore>, DurableStore) {
    let store = LegacyStore::in_memory().expect("in-memory legacy store");
    store.ensure_actor("local-user").expect("actor");
    let store = Arc::new(store);
    let durable = DurableStore::in_memory().expect("durable");
    let config = ServerConfig {
        legacy_store: Some(store.clone()),
        ..ServerConfig::default()
    };
    let server = AppServer::with_agent_runtime(
        config,
        durable.clone(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(workspace).expect("workspace"),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .expect("server");
    (server, store, durable)
}

/// One canonical envelope for direct `DurableStore` appends — mirrors the
/// `durable.rs` helper so tests can seed runs the mirror never saw.
fn durable_event(session_id: &str, run_id: &str, seq: u64, event: CanonicalEvent) -> EventEnvelope {
    EventEnvelope {
        event_id: format!("{run_id}-event-{seq}"),
        schema_version: V1Version::VALUE,
        session_id: session_id.to_owned(),
        run_id: run_id.to_owned(),
        item_id: None,
        seq,
        occurred_at: "2026-09-01T00:00:00Z".to_owned(),
        actor: ActorRef {
            id: "local-user".to_owned(),
            kind: ActorKind::LocalUser,
        },
        source: "test".to_owned(),
        causation_id: None,
        correlation_id: None,
        event,
        extensions: Default::default(),
    }
}

async fn connected_client(
    server: AppServer,
) -> (AppClient, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client.initialize("chat-cutover", "1").await.expect("init");
    (client, task)
}

async fn seed_conversation(store: &LegacyStore) -> i64 {
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Legacy chat".to_owned()),
                working_directory: Some("C:/work".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let messages = [
        NewMessage {
            role: "user".to_owned(),
            content: Some("hello".to_owned()),
            ..NewMessage::default()
        },
        NewMessage {
            role: "assistant".to_owned(),
            content: Some("hi".to_owned()),
            thinking: Some("reasoning".to_owned()),
            ..NewMessage::default()
        },
        NewMessage {
            role: "assistant".to_owned(),
            content: None,
            tool_calls: Some(json!([{
                "id": "call-1",
                "type": "function",
                "name": "read_file",
                "arguments": {"path": "a.txt"},
            }])),
            ..NewMessage::default()
        },
        NewMessage {
            role: "tool".to_owned(),
            content: Some("contents".to_owned()),
            tool_result: Some(json!({
                "tool_call_id": "call-1",
                "name": "read_file",
                "result": {"output": "contents", "is_error": false},
            })),
            ..NewMessage::default()
        },
    ];
    for message in messages {
        store
            .add_message("local-user", conversation.id, &message)
            .expect("message");
    }
    conversation.id
}

fn link_command(conversation_id: i64) -> Command {
    Command::SessionForConversation(SessionForConversationParams {
        idempotency_key: key("link-1"),
        conversation_id,
        session_id: None,
    })
}

async fn request(client: &AppClient, command: Command) -> ResponsePayload {
    client.request(command).await.expect("command succeeds")
}

async fn drain_run(
    mut events: tokio::sync::broadcast::Receiver<EventEnvelope>,
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

async fn drain_terminal(
    mut events: tokio::sync::broadcast::Receiver<EventEnvelope>,
) -> Vec<EventEnvelope> {
    let mut collected = Vec::new();
    loop {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event arrival timeout")
            .expect("event channel open");
        let terminal = matches!(
            envelope.event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        );
        collected.push(envelope);
        if terminal {
            return collected;
        }
    }
}

#[tokio::test]
async fn session_for_conversation_imports_history_and_is_idempotent() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let conversation_id = seed_conversation(&store).await;

    let linked = request(&client, link_command(conversation_id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload: {linked:?}");
    };
    assert_eq!(link.conversation_id, conversation_id);
    assert!(link.created);
    assert!(!link.truncated);
    // Four legacy rows project to five events: the assistant row with
    // `thinking` yields a reasoning delta plus its item.
    assert_eq!(link.imported_events, 5);

    let history = request(
        &client,
        Command::SessionHistory(SessionHistoryParams {
            session_id: link.session_id.clone(),
            limit: 50,
            before_cursor: None,
        }),
    )
    .await;
    let ResponsePayload::SessionHistory(history) = history else {
        panic!("unexpected payload");
    };
    assert!(!history.has_more);
    assert_eq!(history.items.len(), 4, "{:?}", history.items);
    assert_eq!(history.items[0].role, "user");
    assert_eq!(history.items[0].content.as_deref(), Some("hello"));
    assert_eq!(history.items[1].role, "assistant");
    assert_eq!(history.items[1].content.as_deref(), Some("hi"));
    assert_eq!(history.items[1].reasoning.as_deref(), Some("reasoning"));
    assert_eq!(history.items[2].role, "assistant");
    assert_eq!(history.items[2].tool_calls.len(), 1);
    assert_eq!(history.items[2].tool_calls[0].call_id, "call-1");
    assert_eq!(history.items[2].tool_calls[0].name, "read_file");
    assert_eq!(history.items[3].role, "tool");
    assert_eq!(history.items[3].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(history.items[3].name.as_deref(), Some("read_file"));

    let replay = request(&client, link_command(conversation_id)).await;
    let ResponsePayload::SessionForConversation(replay) = replay else {
        panic!("unexpected payload");
    };
    assert_eq!(replay, link, "same idempotency key replays the outcome");

    let rebound = request(
        &client,
        Command::SessionForConversation(SessionForConversationParams {
            idempotency_key: key("link-2"),
            conversation_id,
            session_id: None,
        }),
    )
    .await;
    let ResponsePayload::SessionForConversation(rebound) = rebound else {
        panic!("unexpected payload");
    };
    assert!(!rebound.created);
    assert_eq!(rebound.session_id, link.session_id);
    assert_eq!(rebound.imported_events, 0);
    assert!(!rebound.truncated);

    let runs = request(
        &client,
        Command::SessionRuns(SessionRunsParams {
            session_id: link.session_id.clone(),
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::SessionRuns(runs) = runs else {
        panic!("unexpected payload");
    };
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(runs.runs[0].status, "completed");
    assert_eq!(runs.runs[0].finish_reason.as_deref(), Some("import"));
    // run.started + 5 projected events + run.completed.
    assert_eq!(runs.runs[0].last_seq, 7);

    let events = request(
        &client,
        Command::RunEvents(RunEventsParams {
            run_id: runs.runs[0].run_id.clone(),
            after_seq: None,
            limit: 50,
        }),
    )
    .await;
    let ResponsePayload::EventPage(events) = events else {
        panic!("unexpected payload");
    };
    assert!(events.events.iter().all(|event| {
        event.occurred_at.ends_with('Z')
            && event.occurred_at.contains('T')
            && !event.occurred_at.contains(' ')
    }));
}

#[tokio::test]
async fn legacy_transcript_stays_readable_through_conversations_messages() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let conversation_id = seed_conversation(&store).await;

    let messages = request(
        &client,
        Command::ConversationsMessages(ConversationMessagesParams {
            id: conversation_id,
            before_id: None,
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::ConversationsMessages(messages) = messages else {
        panic!("unexpected payload");
    };
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, "user");
    assert!(messages.iter().any(|message| message.tool_result.is_some()));

    let cursor = messages[2].id;
    let head = request(
        &client,
        Command::ConversationsMessages(ConversationMessagesParams {
            id: conversation_id,
            before_id: Some(cursor),
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::ConversationsMessages(head) = head else {
        panic!("unexpected payload");
    };
    assert_eq!(head.len(), 2);
    assert!(head.iter().all(|message| message.id < cursor));

    let missing = client
        .request(Command::ConversationsMessages(ConversationMessagesParams {
            id: 9_999,
            before_id: None,
            limit: 10,
        }))
        .await
        .expect_err("unknown conversation must fail");
    match missing {
        cool_app_server::ClientError::Protocol(protocol) => {
            assert_eq!(protocol.cool_code, "conversation_not_found");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn failed_and_malformed_tool_rows_degrade_to_canonical_events() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Tools".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let assistant = store
        .add_message(
            "local-user",
            conversation.id,
            &NewMessage {
                role: "assistant".to_owned(),
                content: None,
                tool_calls: Some(json!([
                    {
                        "id": "call-ok",
                        "type": "function",
                        "name": "read_file",
                        "arguments": {"path": "a.txt"},
                    },
                    {
                        "arguments": "not-an-object",
                    },
                ])),
                ..NewMessage::default()
            },
        )
        .expect("assistant row");
    store
        .add_message(
            "local-user",
            conversation.id,
            &NewMessage {
                role: "tool".to_owned(),
                content: Some("denied".to_owned()),
                tool_result: Some(json!({
                    "tool_call_id": "call-ok",
                    "name": "read_file",
                    "result": {"output": "denied", "is_error": true, "error": "permission denied"},
                })),
                ..NewMessage::default()
            },
        )
        .expect("tool row");

    let linked = request(&client, link_command(conversation.id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };
    // Assistant item, the failed result, and the backfilled failure for the
    // malformed call that never received a persisted result.
    assert_eq!(link.imported_events, 3);
    assert!(!link.truncated);

    let history = request(
        &client,
        Command::SessionHistory(SessionHistoryParams {
            session_id: link.session_id,
            limit: 50,
            before_cursor: None,
        }),
    )
    .await;
    let ResponsePayload::SessionHistory(history) = history else {
        panic!("unexpected payload");
    };
    assert_eq!(history.items.len(), 3);
    let calls = &history.items[0].tool_calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[1].name, "unknown");
    assert_eq!(
        calls[1].call_id,
        format!("legacy-{}-{}-1", conversation.id, assistant.id)
    );
    let tool_item = &history.items[1];
    assert_eq!(tool_item.role, "tool");
    assert_eq!(tool_item.tool_call_id.as_deref(), Some("call-ok"));
    let content = tool_item.content.as_deref().unwrap_or_default();
    assert!(content.contains("legacy_tool_error"), "{content}");
    assert!(content.contains("permission denied"), "{content}");
    let backfilled = &history.items[2];
    assert_eq!(backfilled.role, "tool");
    assert_eq!(
        backfilled.tool_call_id.as_deref(),
        Some(calls[1].call_id.as_str())
    );
    let content = backfilled.content.as_deref().unwrap_or_default();
    assert!(content.contains("legacy_tool_unanswered"), "{content}");
}

#[tokio::test]
async fn replay_after_conversation_delete_returns_the_original_link() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let conversation_id = seed_conversation(&store).await;
    let linked = request(&client, link_command(conversation_id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };
    store
        .delete_conversation("local-user", conversation_id)
        .expect("delete conversation");

    let replay = request(&client, link_command(conversation_id)).await;
    let ResponsePayload::SessionForConversation(replay) = replay else {
        panic!("unexpected payload");
    };
    assert_eq!(replay, link, "idempotent replay survives the source delete");

    let error = client
        .request(Command::SessionForConversation(
            SessionForConversationParams {
                idempotency_key: key("link-after-delete"),
                conversation_id,
                session_id: None,
            },
        ))
        .await
        .expect_err("a fresh binding needs the conversation");
    match error {
        cool_app_server::ClientError::Protocol(protocol) => {
            assert_eq!(protocol.cool_code, "conversation_not_found");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn empty_conversations_link_without_imported_events() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Empty".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");

    let linked = request(&client, link_command(conversation.id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };
    assert!(link.created);
    assert_eq!(link.imported_events, 0);
    assert!(!link.truncated);

    let runs = request(
        &client,
        Command::SessionRuns(SessionRunsParams {
            session_id: link.session_id.clone(),
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::SessionRuns(runs) = runs else {
        panic!("unexpected payload");
    };
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(runs.runs[0].last_seq, 2);

    let history = request(
        &client,
        Command::SessionHistory(SessionHistoryParams {
            session_id: link.session_id,
            limit: 50,
            before_cursor: None,
        }),
    )
    .await;
    let ResponsePayload::SessionHistory(history) = history else {
        panic!("unexpected payload");
    };
    assert!(history.items.is_empty());
}

#[tokio::test]
async fn conversations_messages_rejects_out_of_range_limits() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let conversation_id = seed_conversation(&store).await;
    for limit in [0_u16, 2001] {
        let error = client
            .request(Command::ConversationsMessages(ConversationMessagesParams {
                id: conversation_id,
                before_id: None,
                limit,
            }))
            .await
            .expect_err("out-of-range limit must fail");
        match error {
            cool_app_server::ClientError::Protocol(protocol) => {
                assert_eq!(protocol.cool_code, "invalid_input", "limit {limit}");
            }
            other => panic!("unexpected error for limit {limit}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn chat_commands_fail_closed_without_a_legacy_store() {
    let server = AppServer::new(ServerConfig::default());
    let (client, _task) = connected_client(server).await;

    let error = client
        .request(link_command(1))
        .await
        .expect_err("link must fail closed");
    match error {
        cool_app_server::ClientError::Protocol(protocol) => {
            assert_eq!(protocol.cool_code, "legacy_store_unavailable");
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let error = client
        .request(Command::SessionRuns(SessionRunsParams {
            session_id: "session-missing".to_owned(),
            limit: 0,
        }))
        .await
        .expect_err("zero limit must fail");
    match error {
        cool_app_server::ClientError::Protocol(protocol) => {
            assert_eq!(protocol.cool_code, "invalid_session_runs_limit");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn session_runs_lists_import_and_prompt_runs_newest_first() {
    let directory = tempdir().unwrap();
    let legacy = LegacyStore::in_memory().expect("legacy store");
    legacy.ensure_actor("local-user").expect("actor");
    let legacy = Arc::new(legacy);
    let conversation_id = seed_conversation(&legacy).await;
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("canonical reply".to_owned()),
        ModelEvent::Finish { reason: None },
    ])]));
    let server = AppServer::with_agent_runtime(
        ServerConfig {
            legacy_store: Some(legacy.clone()),
            ..ServerConfig::default()
        },
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, _task) = connected_client(server).await;

    let linked = request(&client, link_command(conversation_id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };

    let events = client.subscribe();
    let run_id = client
        .prompt("prompt-1", &link.session_id, "follow up", None)
        .await
        .expect("prompt accepted")
        .run_id;
    drain_run(events, &run_id).await;

    let runs = request(
        &client,
        Command::SessionRuns(SessionRunsParams {
            session_id: link.session_id.clone(),
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::SessionRuns(runs) = runs else {
        panic!("unexpected payload");
    };
    assert_eq!(runs.runs.len(), 2);
    assert_eq!(runs.runs[0].run_id, run_id);
    assert_eq!(runs.runs[0].status, "completed");
    assert_eq!(runs.runs[1].finish_reason.as_deref(), Some("import"));

    let history = request(
        &client,
        Command::SessionHistory(SessionHistoryParams {
            session_id: link.session_id,
            limit: 50,
            before_cursor: None,
        }),
    )
    .await;
    let ResponsePayload::SessionHistory(history) = history else {
        panic!("unexpected payload");
    };
    let roles = history
        .items
        .iter()
        .map(|item| item.role.as_str())
        .collect::<Vec<_>>();
    assert!(
        roles.len() >= 6,
        "imported turns plus the new prompt: {roles:?}"
    );
    let last = history.items.last().expect("assistant reply");
    assert_eq!(last.role, "assistant");
    assert_eq!(last.content.as_deref(), Some("canonical reply"));
}

#[tokio::test]
async fn session_run_mirrors_into_legacy_inspector_and_analytics() {
    // B2/B7 regression: a canonical session run on a conversation-linked
    // session projects into the legacy run/event/spend tables, so the
    // Inspector run picker and timeline plus the Analytics/Budgets pages see
    // the real data instead of empty rows.
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("done".to_owned()),
        ModelEvent::Usage(cool_agent::Usage {
            prompt_tokens: 12,
            completion_tokens: 8,
            total_tokens: 20,
            cost_micro_usd: Some(42_000),
            ..Default::default()
        }),
        ModelEvent::Finish { reason: None },
    ])]));
    let (server, store, _durable) = scripted_legacy_server(provider, directory.path());
    let (client, _task) = connected_client(server).await;

    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Mirrored run".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let linked = request(&client, link_command(conversation.id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload: {linked:?}");
    };

    let events = client.subscribe();
    let run_id = client
        .prompt("mirror-prompt", &link.session_id, "do the thing", None)
        .await
        .expect("prompt")
        .run_id;
    drain_run(events, &run_id).await;

    let session_runs = request(
        &client,
        Command::SessionRuns(SessionRunsParams {
            session_id: link.session_id.clone(),
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::SessionRuns(session_runs) = session_runs else {
        panic!("unexpected payload");
    };
    assert_eq!(session_runs.runs.len(), 2);
    assert_eq!(session_runs.runs[0].run_id, run_id);
    assert_eq!(
        session_runs.runs[1].finish_reason.as_deref(),
        Some("import")
    );

    let listed = request(
        &client,
        Command::RunsList(RunListParams {
            conversation_id: conversation.id,
            before_id: None,
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::RunsListed(runs) = listed else {
        panic!("unexpected payload");
    };
    assert_eq!(runs.len(), 1);
    let mirrored = &runs[0];
    assert_eq!(mirrored.status, "completed");
    assert_eq!(
        mirrored
            .config
            .as_ref()
            .and_then(|config| config["durableRunId"].as_str()),
        Some(run_id.as_str())
    );
    let usage = mirrored.usage.clone().expect("run usage recorded");
    assert_eq!(usage["total_tokens"].as_f64(), Some(20.0));

    let timeline = request(
        &client,
        Command::InspectorTimeline(LegacyIdParams { id: mirrored.id }),
    )
    .await;
    let ResponsePayload::InspectorTimeline(timeline) = timeline else {
        panic!("unexpected payload");
    };
    let kinds = timeline
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect::<Vec<_>>();
    for expected in ["start", "message", "llm_call_complete", "finish"] {
        assert!(
            kinds.contains(&expected),
            "timeline missing {expected}: {kinds:?}"
        );
    }

    let summary = request(
        &client,
        Command::AnalyticsSummary(AnalyticsDaysParams { days: 30 }),
    )
    .await;
    assert!(matches!(
        summary,
        ResponsePayload::AnalyticsSummary(summary)
            if summary.total_llm_calls == 1 && summary.total_tokens == 20
    ));

    let spend = request(
        &client,
        Command::BudgetsSpend(BudgetSpendParams {
            since: None,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(
        spend,
        ResponsePayload::BudgetsSpend(rows)
            if rows.len() == 1 && rows[0].total_tokens == 20 && rows[0].model == "scripted"
    ));
}

#[tokio::test]
async fn inspector_replay_spawns_a_mirrored_agent_run() {
    // The legacy `inspector.replay` bookkeeping creates the replacement run
    // row; the canonical pipeline then executes it and the mirror fills the
    // row in — matching what the Inspector Replay button drives.
    let directory = tempdir().unwrap();
    let script = || {
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish { reason: None },
        ])
    };
    let provider = Arc::new(ScriptedDriver::new([script(), script()]));
    let (server, store, _durable) = scripted_legacy_server(provider, directory.path());
    let (client, _task) = connected_client(server).await;

    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Replay source".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let linked = request(&client, link_command(conversation.id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload: {linked:?}");
    };

    let events = client.subscribe();
    let run_id = client
        .prompt("replay-source", &link.session_id, "original prompt", None)
        .await
        .expect("prompt")
        .run_id;
    drain_run(events, &run_id).await;
    let original = store
        .find_run_by_durable_id("local-user", conversation.id, &run_id)
        .expect("lookup")
        .expect("mirrored original");

    let events = client.subscribe();
    let replayed = request(
        &client,
        Command::InspectorReplay(ReplayParams {
            idempotency_key: key("replay-mirror-1"),
            run_id: original.id,
            model: None,
            system_prompt: None,
            temperature: None,
        }),
    )
    .await;
    let ResponsePayload::InspectorReplayed(replay) = replayed else {
        panic!("unexpected payload: {replayed:?}");
    };
    assert_eq!(replay.original_run_id, original.id);
    assert_ne!(replay.new_run_id, original.id);
    assert_eq!(replay.status, "running");

    // The canonical replay run executes on its own durable run id; the
    // mirror binds the pre-created legacy row to it and drives it to a
    // terminal status.
    let envelopes = drain_terminal(events).await;
    let aux_run_id = envelopes
        .last()
        .map(|envelope| envelope.run_id.clone())
        .expect("terminal envelope");
    let row = store
        .get_run("local-user", replay.new_run_id)
        .expect("replay row");
    assert_eq!(row.status, "completed");
    assert_eq!(
        row.config
            .as_ref()
            .and_then(|config| config["durableRunId"].as_str()),
        Some(aux_run_id.as_str())
    );

    // Repeating the call replays the stored idempotent record instead of
    // spawning a second execution.
    let again = request(
        &client,
        Command::InspectorReplay(ReplayParams {
            idempotency_key: key("replay-mirror-1"),
            run_id: original.id,
            model: None,
            system_prompt: None,
            temperature: None,
        }),
    )
    .await;
    let ResponsePayload::InspectorReplayed(again) = again else {
        panic!("unexpected payload: {again:?}");
    };
    assert_eq!(again.new_run_id, replay.new_run_id);
    assert_eq!(
        store
            .list_runs("local-user", conversation.id, &RunFilter::default())
            .expect("runs")
            .len(),
        2
    );
}

/// `session.rewind` copies the retained history into the seed run with a
/// `rewound` extension; the mirror emits those copies' timeline rows but
/// must not re-run the spend/tool/usage accounting the superseded run
/// already recorded.
#[tokio::test]
async fn rewind_mirrors_copied_history_without_double_counting() {
    let directory = tempdir().expect("tempdir");
    std::fs::write(directory.path().join("note.txt"), "contents").expect("fixture file");
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![ModelEvent::ToolCall(ToolCall {
            call_id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            arguments: json!({"path": "note.txt"})
                .as_object()
                .expect("arguments")
                .clone(),
        })]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Usage(cool_agent::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost_micro_usd: Some(30_000),
                ..Default::default()
            }),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
    let (server, store, durable) = scripted_legacy_server(provider, directory.path());
    let (client, _task) = connected_client(server).await;

    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Rewind".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let linked = request(&client, link_command(conversation.id)).await;
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload: {linked:?}");
    };

    let events = client.subscribe();
    let run_id = client
        .prompt("rewind-prompt", &link.session_id, "read the note", None)
        .await
        .expect("prompt")
        .run_id;
    drain_run(events, &run_id).await;
    assert_eq!(
        store
            .list_tool_calls("local-user", Some(conversation.id), None)
            .expect("tool calls")
            .len(),
        1
    );
    assert_eq!(
        store
            .list_spend("local-user", None, None)
            .expect("spend")
            .len(),
        1
    );

    // Rewind to just before `run.completed`: the retained window holds the
    // `usage.updated` event too. It is not in `is_history_event`'s copied
    // set today, so it stays un-copied — the tagged-copy assertions below
    // still lock the accounting side for the copied window it sits in.
    let window = durable
        .session_event_window(&link.session_id, "local-user", None, usize::MAX)
        .expect("window");
    let cursor = window
        .iter()
        .find(|(_, envelope)| matches!(envelope.event, CanonicalEvent::UsageUpdated(_)))
        .map(|(rowid, _)| *rowid)
        .expect("usage.updated cursor");
    let rewound = request(
        &client,
        Command::SessionRewind(SessionRewindParams {
            idempotency_key: key("rewind-1"),
            session_id: link.session_id.clone(),
            to_cursor: cursor,
            reason: None,
            restore_workspace: None,
        }),
    )
    .await;
    let ResponsePayload::SessionRewound(rewind) = rewound else {
        panic!("unexpected payload: {rewound:?}");
    };
    assert!(rewind.rewound_run_ids.contains(&run_id));

    // The superseded row keeps its recorded terminal state — it genuinely
    // completed in history; the close path exists to repair rows left open.
    let old = store
        .find_run_by_durable_id("local-user", conversation.id, &run_id)
        .expect("find")
        .expect("old row");
    assert_eq!(old.status, "completed");
    assert_eq!(
        old.usage
            .as_ref()
            .and_then(|usage| usage["total_tokens"].as_f64()),
        Some(15.0)
    );

    let seed = store
        .find_run_by_durable_id("local-user", conversation.id, &rewind.run_id)
        .expect("find")
        .expect("seed row");
    assert_eq!(seed.status, "completed");
    assert_eq!(seed.usage, None, "copied usage must not re-count");

    let timeline = store
        .list_run_events("local-user", seed.id, None, Some(100))
        .expect("timeline");
    let kinds: Vec<&str> = timeline.iter().map(|event| event.kind.as_str()).collect();
    assert!(
        kinds.contains(&"tool_result"),
        "the tagged tool.completed copy still mirrors its timeline row: {kinds:?}"
    );
    assert!(
        kinds.contains(&"message"),
        "the tagged message copies still mirror: {kinds:?}"
    );
    assert_eq!(
        store
            .list_tool_calls("local-user", Some(conversation.id), None)
            .expect("tool calls")
            .len(),
        1,
        "rewind must not duplicate the tool_calls row"
    );
    assert_eq!(
        store
            .list_spend("local-user", None, None)
            .expect("spend")
            .len(),
        1,
        "rewind must not duplicate the spend_log row"
    );
}

/// A run abandoned mid-`running` by a restart is failed by
/// `recover_incomplete_runs`; the envelopes it emits reach the mirror
/// through `with_store` so the legacy row closes instead of staying a
/// zombie `running` picker entry.
#[test]
fn recovered_crash_run_closes_its_mirrored_row() {
    let store = Arc::new(LegacyStore::in_memory().expect("store"));
    store.ensure_actor("local-user").expect("actor");
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Crash".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let durable = DurableStore::in_memory().expect("durable");
    let link = durable
        .link_conversation(
            "local-user",
            "link",
            "fp",
            conversation.id,
            None,
            None,
            &[],
            false,
        )
        .expect("link");
    let run_id = durable
        .start_run("local-user", "run", "fp", &link.session_id)
        .expect("run")
        .value;
    durable
        .append_event(
            "local-user",
            &durable_event(
                &link.session_id,
                &run_id,
                1,
                CanonicalEvent::RunStarted(RunStarted {
                    model: Some("scripted".to_owned()),
                    mode: None,
                }),
            ),
        )
        .expect("started");

    // Server construction runs crash recovery: the run is failed and the
    // failure envelope is mirrored.
    let _server = AppServer::with_store(
        ServerConfig {
            legacy_store: Some(store.clone()),
            ..ServerConfig::default()
        },
        durable,
    )
    .expect("server");

    let row = store
        .find_run_by_durable_id("local-user", conversation.id, &run_id)
        .expect("find")
        .expect("mirrored row");
    assert_eq!(row.status, "failed");
    assert!(row.finished_at.is_some(), "recovered run must close");
}

/// Canonical runs that predate the mirror (or missed a projection) are
/// backfilled by the startup reconciliation sweep — the picker and the
/// analytics rows appear without replaying the prompt.
#[test]
fn startup_sweep_backfills_unmirrored_runs() {
    let store = Arc::new(LegacyStore::in_memory().expect("store"));
    store.ensure_actor("local-user").expect("actor");
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Sweep".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let durable = DurableStore::in_memory().expect("durable");
    let link = durable
        .link_conversation(
            "local-user",
            "link",
            "fp",
            conversation.id,
            None,
            None,
            &[],
            false,
        )
        .expect("link");
    let run_id = durable
        .start_run("local-user", "run", "fp", &link.session_id)
        .expect("run")
        .value;
    // `rewound` marks the envelope as a retained-history copy (what
    // `rewind_session` stamps on cloned events): it projects only its
    // timeline row, never spend/tool/usage accounting. Exercised here
    // directly because `usage.updated` is not in the copied set
    // `is_history_event` uses today — this locks the invariant for any
    // copy, whatever the set becomes.
    for (seq, (event, rewound)) in [
        (
            CanonicalEvent::RunStarted(RunStarted {
                model: Some("scripted".to_owned()),
                mode: None,
            }),
            false,
        ),
        (
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: Some("backfilled".to_owned()),
                tool_calls: Vec::new(),
            }),
            false,
        ),
        (
            CanonicalEvent::UsageUpdated(UsageUpdated {
                prompt_tokens: 4,
                completion_tokens: 6,
                total_tokens: 10,
                cost_usd: Some(0.001),
            }),
            false,
        ),
        (
            CanonicalEvent::ToolCompleted(ToolCompleted {
                call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                result: json!({"bytes": 8}),
            }),
            true,
        ),
        (
            CanonicalEvent::UsageUpdated(UsageUpdated {
                prompt_tokens: 60,
                completion_tokens: 39,
                total_tokens: 99,
                cost_usd: Some(0.009),
            }),
            true,
        ),
        (
            CanonicalEvent::RunCompleted(RunTerminal {
                reason: "stop".to_owned(),
                error_code: None,
            }),
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut envelope = durable_event(&link.session_id, &run_id, seq as u64 + 1, event);
        if rewound {
            envelope
                .extensions
                .insert("rewound".to_owned(), json!(true));
        }
        durable
            .append_event("local-user", &envelope)
            .expect("append");
    }

    let _server = AppServer::with_store(
        ServerConfig {
            legacy_store: Some(store.clone()),
            ..ServerConfig::default()
        },
        durable,
    )
    .expect("server");

    let row = store
        .find_run_by_durable_id("local-user", conversation.id, &run_id)
        .expect("find")
        .expect("swept row");
    assert_eq!(row.status, "completed");
    assert_eq!(
        row.usage
            .as_ref()
            .and_then(|usage| usage["total_tokens"].as_f64()),
        Some(10.0),
        "the tagged usage copy must not add to run usage"
    );
    assert_eq!(
        store
            .list_spend("local-user", None, None)
            .expect("spend")
            .len(),
        1,
        "the tagged usage copy must not add a spend_log row"
    );
    assert!(
        store
            .list_tool_calls("local-user", Some(conversation.id), None)
            .expect("tool calls")
            .is_empty(),
        "the tagged tool.completed copy must not add a tool_calls row"
    );
    let kinds: Vec<String> = store
        .list_run_events("local-user", row.id, None, Some(100))
        .expect("timeline")
        .iter()
        .map(|event| event.kind.clone())
        .collect();
    for expected in [
        "start",
        "message",
        "llm_call_complete",
        "tool_result",
        "finish",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "{expected} in {kinds:?}"
        );
    }
}

/// The sweep must not resurrect the aux-run noise the mirror deliberately
/// skips: compaction and subagent-lifecycle bookkeeping runs belong to
/// their own domains — a restart must not change what the picker or
/// Analytics show. `replay_exec` runs stay user-facing and keep mirroring.
#[test]
fn startup_sweep_skips_auxiliary_runs() {
    let store = Arc::new(LegacyStore::in_memory().expect("store"));
    store.ensure_actor("local-user").expect("actor");
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Aux sweep".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let durable = DurableStore::in_memory().expect("durable");
    let link = durable
        .link_conversation(
            "local-user",
            "link",
            "fp",
            conversation.id,
            None,
            None,
            &[],
            false,
        )
        .expect("link");
    let terminal = |reason: &str| {
        CanonicalEvent::RunCompleted(RunTerminal {
            reason: reason.to_owned(),
            error_code: None,
        })
    };
    let usage = || {
        CanonicalEvent::UsageUpdated(UsageUpdated {
            prompt_tokens: 2,
            completion_tokens: 1,
            total_tokens: 3,
            cost_usd: Some(0.001),
        })
    };
    let seed_aux = |purpose: &str, reason: &str| {
        let run_id = durable
            .start_auxiliary_run("local-user", &link.session_id, purpose)
            .expect("aux run");
        for (seq, event) in [usage(), terminal(reason)].into_iter().enumerate() {
            durable
                .append_event(
                    "local-user",
                    &durable_event(&link.session_id, &run_id, seq as u64 + 1, event),
                )
                .expect("append");
        }
        run_id
    };
    let subagent_run = seed_aux("subagent", "subagent_completed");
    let compact_run = seed_aux("compact", "compact");
    let replay_run = seed_aux("replay_exec", "stop");

    let _server = AppServer::with_store(
        ServerConfig {
            legacy_store: Some(store.clone()),
            ..ServerConfig::default()
        },
        durable,
    )
    .expect("server");

    for run_id in [&subagent_run, &compact_run] {
        assert!(
            store
                .find_run_by_durable_id("local-user", conversation.id, run_id)
                .expect("find")
                .is_none(),
            "aux run {run_id} must not produce a picker row"
        );
    }
    assert!(
        store
            .find_run_by_durable_id("local-user", conversation.id, &replay_run)
            .expect("find")
            .is_some(),
        "replay exec runs are user-facing and must still mirror"
    );
    assert_eq!(
        store
            .list_spend("local-user", None, None)
            .expect("spend")
            .len(),
        1,
        "only the replay exec run's usage may reach spend_log"
    );
}
