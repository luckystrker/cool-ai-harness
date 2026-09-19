//! Conversations/messages store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::conversations::{
    ConversationFilter, ConversationPatch, MessagePage, NewConversation, NewMessage,
};
use cool_store::{LegacyStore, StoreError};
use serde_json::json;
use tempfile::TempDir;

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let store = LegacyStore::open(
        &path,
        &cool_store::StoreOptions {
            initialize_if_missing: false,
            ..cool_store::StoreOptions::default()
        },
    )
    .expect("adopt");
    (directory, store)
}

#[test]
fn conversation_crud_round_trips_through_the_legacy_schema() {
    let (_directory, store) = adopted_store();

    let created = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("New chat".to_string()),
                model: Some("test-model".to_string()),
                permissions: Some(json!({"network": "ask"})),
                tags: Some(json!(["alpha"])),
                ..NewConversation::default()
            },
        )
        .expect("create");
    assert_eq!(created.title.as_deref(), Some("New chat"));
    assert_eq!(created.permissions, Some(json!({"network": "ask"})));

    let updated = store
        .update_conversation(
            "local-user",
            created.id,
            &ConversationPatch {
                title: Some("Renamed".to_string()),
                is_pinned: Some(true),
                folder: Some("inbox".to_string()),
                working_directory: Some(String::new()),
                ..ConversationPatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.title.as_deref(), Some("Renamed"));
    assert!(updated.is_pinned);
    assert_eq!(updated.folder.as_deref(), Some("inbox"));
    assert_eq!(updated.working_directory, None);

    let listed = store
        .list_conversations(
            "local-user",
            &ConversationFilter {
                folder: Some("inbox".to_string()),
                ..ConversationFilter::default()
            },
        )
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    store
        .delete_conversation("local-user", created.id)
        .expect("delete");
    let error = store
        .get_conversation("local-user", created.id)
        .expect_err("gone");
    assert!(matches!(error, StoreError::NotFound("conversation")));
}

#[test]
fn conversations_are_actor_scoped_and_machine_owned_rows_are_hidden() {
    let (_directory, store) = adopted_store();
    store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Visible".to_string()),
                ..NewConversation::default()
            },
        )
        .expect("visible");
    store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Subagent run".to_string()),
                metadata: Some(json!({"is_subagent": true})),
                ..NewConversation::default()
            },
        )
        .expect("machine owned");

    let visible = store
        .list_conversations("local-user", &ConversationFilter::default())
        .expect("list");
    assert_eq!(visible.len(), 2, "legacy chat plus visible conversation");
    assert!(
        visible
            .iter()
            .all(|row| row.title.as_deref() != Some("Subagent run"))
    );

    store.ensure_actor("other-actor").expect("register actor");
    store
        .create_conversation("other-actor", &NewConversation::default())
        .expect("second actor");
    let other = store
        .list_conversations("other-actor", &ConversationFilter::default())
        .expect("list");
    assert_eq!(other.len(), 1);
    let error = store
        .get_conversation("other-actor", 1)
        .expect_err("cross-actor read must fail");
    assert!(matches!(error, StoreError::NotFound("conversation")));
}

#[test]
fn message_pages_are_chronological_and_cursor_bounded() {
    let (_directory, store) = adopted_store();
    for role in ["user", "assistant", "user"] {
        store
            .add_message(
                "local-user",
                1,
                &NewMessage {
                    role: role.to_string(),
                    content: Some(format!("{role} turn")),
                    usage: Some(json!({"total_tokens": 3})),
                    ..NewMessage::default()
                },
            )
            .expect("append");
    }
    let all = store
        .list_messages("local-user", 1, &MessagePage::default())
        .expect("list");
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].content.as_deref(), Some("hello"));

    let after = store
        .list_messages(
            "local-user",
            1,
            &MessagePage {
                after_id: Some(all[1].id),
                ..MessagePage::default()
            },
        )
        .expect("after");
    assert_eq!(after.len(), 2);

    let last = all.last().expect("last");
    let updated = store
        .update_message_content(
            "local-user",
            1,
            last.id,
            Some("final"),
            Some("thinking"),
            Some(&json!({"total_tokens": 7})),
            Some(42),
        )
        .expect("update");
    assert_eq!(updated.content.as_deref(), Some("final"));
    assert_eq!(updated.thinking.as_deref(), Some("thinking"));
    assert_eq!(updated.duration_ms, Some(42));
}

#[test]
fn deleting_a_conversation_with_history_matches_python_orphaning() {
    let (_directory, store) = adopted_store();
    // Give the conversation a run, an event, an artifact and a tool call so
    // dependent rows exist, then delete it exactly like the Python service
    // (messages + conversation only). Foreign keys are off on both runtimes,
    // so the historical rows stay behind instead of blocking the delete.
    let run = store
        .create_run("local-user", 1, &Default::default())
        .expect("run");
    store
        .append_run_event("local-user", run.id, "run.started", None)
        .expect("event");
    store
        .record_tool_call(
            "local-user",
            &cool_store::domains::runs::NewToolCall {
                conversation_id: Some(1),
                message_id: None,
                name: "read_file".to_string(),
                arguments: None,
                result: None,
                duration_ms: None,
                success: true,
                error: None,
            },
        )
        .expect("tool call");

    store.delete_conversation("local-user", 1).expect("delete");

    let connection = rusqlite::Connection::open(store.path().expect("path")).expect("open");
    let runs_left: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE conversation_id = 1",
            [],
            |row| row.get(0),
        )
        .expect("count runs");
    assert_eq!(runs_left, 1, "historical run rows are orphaned like Python");
    let error = store
        .list_messages("local-user", 1, &MessagePage::default())
        .expect_err("conversation gone");
    assert!(matches!(error, StoreError::NotFound("conversation")));
}

#[test]
fn conversation_search_matches_titles_and_message_content() {
    let (_directory, store) = adopted_store();
    store
        .add_message(
            "local-user",
            1,
            &NewMessage {
                role: "assistant".to_string(),
                content: Some("the quick brown fox".to_string()),
                ..NewMessage::default()
            },
        )
        .expect("append");

    let by_content = store
        .list_conversations(
            "local-user",
            &ConversationFilter {
                search: Some("brown fox".to_string()),
                ..ConversationFilter::default()
            },
        )
        .expect("search content");
    assert_eq!(by_content.len(), 1);

    let by_title = store
        .list_conversations(
            "local-user",
            &ConversationFilter {
                search: Some("Legacy".to_string()),
                ..ConversationFilter::default()
            },
        )
        .expect("search title");
    assert_eq!(by_title.len(), 1);

    let no_match = store
        .list_conversations(
            "local-user",
            &ConversationFilter {
                search: Some("absent".to_string()),
                ..ConversationFilter::default()
            },
        )
        .expect("search absent");
    assert!(no_match.is_empty());
}
