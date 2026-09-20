//! M11 canonical conversation compaction coverage: `conversations.compact`
//! summarizes older canonical items with the provider runtime, writes the
//! legacy `working_memory`, and projects a `session.compacted` summary item.

use std::sync::Arc;
use std::time::Duration;

use cool_agent::{AgentRuntime, ModelEvent, ScriptedDriver, builtin_registry};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::*;
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::domains::conversations::{NewConversation, NewMessage};
use tempfile::tempdir;

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("valid key")
}

struct Harness {
    client: AppClient,
    store: Arc<LegacyStore>,
    _task: tokio::task::JoinHandle<std::io::Result<()>>,
    _directory: tempfile::TempDir,
}

async fn harness(driver: Arc<ScriptedDriver>) -> Harness {
    let directory = tempdir().expect("tempdir");
    let store = Arc::new(LegacyStore::in_memory().expect("legacy store"));
    store.ensure_actor("local-user").expect("actor");
    let config = ServerConfig {
        legacy_store: Some(store.clone()),
        event_delay: Duration::ZERO,
        ..ServerConfig::default()
    };
    let server = AppServer::with_agent_runtime(
        config,
        DurableStore::in_memory().expect("durable"),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(directory.path()).expect("workspace"),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .expect("server");
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client.initialize("compact-tests", "1").await.expect("init");
    Harness {
        client,
        store,
        _task: task,
        _directory: directory,
    }
}

fn seed_conversation(store: &LegacyStore, messages: usize) -> i64 {
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Long".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    for index in 0..messages {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        store
            .add_message(
                "local-user",
                conversation.id,
                &NewMessage {
                    role: role.to_owned(),
                    content: Some(format!("message {index}")),
                    ..NewMessage::default()
                },
            )
            .expect("message");
    }
    conversation.id
}

async fn link(client: &AppClient, conversation_id: i64) -> String {
    let linked = client
        .request(Command::SessionForConversation(
            SessionForConversationParams {
                idempotency_key: key(&format!("link-{conversation_id}")),
                conversation_id,
            },
        ))
        .await
        .expect("link");
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };
    link.session_id
}

fn summary_driver() -> Arc<ScriptedDriver> {
    Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("ROLLING SUMMARY".to_owned()),
        ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        },
    ])]))
}

#[tokio::test]
async fn compact_summarizes_and_projects_a_summary_item() {
    let harness = harness(summary_driver()).await;
    let conversation_id = seed_conversation(&harness.store, 35);
    let session_id = link(&harness.client, conversation_id).await;

    let compacted = harness
        .client
        .request(Command::ConversationsCompact(IdempotentIdParams {
            idempotency_key: key("compact"),
            id: conversation_id,
        }))
        .await
        .expect("compact");
    let ResponsePayload::ConversationsCompacted(result) = compacted else {
        panic!("unexpected payload");
    };
    assert_eq!(result.status, "compacted");
    assert_eq!(result.messages_kept, Some(10));
    assert_eq!(result.messages_compacted, Some(25));
    assert_eq!(result.summary_length, Some("ROLLING SUMMARY".len() as u64));

    // Legacy working memory keeps the summary (parity).
    let memory = harness
        .store
        .get_working_memory("local-user", conversation_id)
        .expect("memory")
        .expect("working memory");
    assert_eq!(memory.summary.as_deref(), Some("ROLLING SUMMARY"));

    // The canonical transcript projects a summary item with the cutoff.
    let history = harness
        .client
        .request(Command::SessionHistory(SessionHistoryParams {
            session_id,
            limit: 100,
            before_cursor: None,
        }))
        .await
        .expect("history");
    let ResponsePayload::SessionHistory(history) = history else {
        panic!("unexpected payload");
    };
    let summary = history
        .items
        .iter()
        .find(|item| item.role == "summary")
        .expect("summary item");
    assert_eq!(summary.content.as_deref(), Some("ROLLING SUMMARY"));
    assert!(summary.compact_up_to_cursor.is_some());
}

#[tokio::test]
async fn compact_is_idempotent_and_skips_short_conversations() {
    let harness = harness(summary_driver()).await;

    // Too few messages to compact.
    let short = seed_conversation(&harness.store, 12);
    link(&harness.client, short).await;
    let skipped = harness
        .client
        .request(Command::ConversationsCompact(IdempotentIdParams {
            idempotency_key: key("compact-short"),
            id: short,
        }))
        .await
        .expect("compact");
    let ResponsePayload::ConversationsCompacted(result) = skipped else {
        panic!("unexpected payload");
    };
    assert_eq!(result.status, "skipped");

    // A replay returns the stored result without a second summary.
    let conversation_id = seed_conversation(&harness.store, 35);
    link(&harness.client, conversation_id).await;
    let first = harness
        .client
        .request(Command::ConversationsCompact(IdempotentIdParams {
            idempotency_key: key("compact-once"),
            id: conversation_id,
        }))
        .await
        .expect("compact");
    let ResponsePayload::ConversationsCompacted(first) = first else {
        panic!("unexpected payload");
    };
    let replay = harness
        .client
        .request(Command::ConversationsCompact(IdempotentIdParams {
            idempotency_key: key("compact-once"),
            id: conversation_id,
        }))
        .await
        .expect("replay");
    let ResponsePayload::ConversationsCompacted(replay) = replay else {
        panic!("unexpected payload");
    };
    assert_eq!(first.messages_compacted, replay.messages_compacted);

    // Exactly the threshold compacts (Python runs when `len < threshold` is
    // false), and a second compaction has no fresh items.
    let boundary = seed_conversation(&harness.store, 30);
    link(&harness.client, boundary).await;
    let compacted = harness
        .client
        .request(Command::ConversationsCompact(IdempotentIdParams {
            idempotency_key: key("compact-boundary"),
            id: boundary,
        }))
        .await
        .expect("compact");
    let ResponsePayload::ConversationsCompacted(compacted) = compacted else {
        panic!("unexpected payload");
    };
    assert_eq!(compacted.status, "compacted");
    // The second compaction has already summarized every older item.
    let again = harness
        .client
        .request(Command::ConversationsCompact(IdempotentIdParams {
            idempotency_key: key("compact-again"),
            id: boundary,
        }))
        .await
        .expect("compact");
    let ResponsePayload::ConversationsCompacted(again) = again else {
        panic!("unexpected payload");
    };
    assert_eq!(again.status, "skipped");
    assert_eq!(again.reason.as_deref(), Some("No new messages to compact"));
}
