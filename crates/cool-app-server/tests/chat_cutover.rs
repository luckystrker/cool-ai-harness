//! M11 chat-cutover foundations: legacy conversation -> durable session link,
//! canonical transcript import, and bounded reads for the React surface.
//!
//! These tests drive the real JSON-RPC client against an `AppServer` backed by
//! an in-memory legacy store plus the in-memory durable store, so the exercised
//! path is exactly the one `cool serve` uses.

use std::sync::Arc;
use std::time::Duration;

use cool_agent::{AgentRuntime, ModelEvent, ScriptedDriver, builtin_registry};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::*;
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::domains::conversations::{NewConversation, NewMessage};
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
