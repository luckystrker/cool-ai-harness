//! Subagent role and run store parity tests (Фаза 2 §5).

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::conversations::{ConversationPatch, NewConversation};
use cool_store::domains::subagents::{
    NewSubagentRole, NewSubagentRun, SubagentRolePatch, SubagentRunFilter,
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

fn seed_run(store: &LegacyStore, role_id: Option<i64>, prompt: &str) -> i64 {
    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("[Subagent] task".to_string()),
                metadata: Some(json!({"is_subagent": true})),
                ..NewConversation::default()
            },
        )
        .expect("isolated conversation");
    store
        .create_subagent_run(
            "local-user",
            1,
            &NewSubagentRun {
                role_id,
                conversation_id: conversation.id,
                name: Some("worker".to_string()),
                prompt: prompt.to_string(),
                ..NewSubagentRun::default()
            },
        )
        .expect("run")
        .id
}

#[test]
fn subagent_role_crud_ordering_and_builtin_policy() {
    let (_directory, store) = adopted_store();
    let custom = store
        .create_subagent_role(&NewSubagentRole {
            name: "zeta".to_string(),
            description: Some("custom".to_string()),
            tool_names: Some(json!(["read_file"])),
            max_cost_usd: Some(1.5),
            ..NewSubagentRole::default()
        })
        .expect("create custom");
    assert_eq!(custom.max_iterations, 10);
    assert!(!custom.is_builtin);

    let builtin = store
        .create_subagent_role(&NewSubagentRole {
            name: "alpha".to_string(),
            max_iterations: 20,
            is_builtin: true,
            ..NewSubagentRole::default()
        })
        .expect("create builtin");

    let roles = store.list_subagent_roles().expect("list");
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0].id, builtin.id);
    assert_eq!(roles[0].name, "alpha");

    let updated = store
        .update_subagent_role(
            custom.id,
            &SubagentRolePatch {
                description: Some("updated".to_string()),
                max_iterations: Some(3),
                max_cost_usd: Some(None),
                ..SubagentRolePatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.description.as_deref(), Some("updated"));
    assert_eq!(updated.max_iterations, 3);
    assert_eq!(updated.max_cost_usd, None);

    let protected = store
        .delete_subagent_role(builtin.id)
        .expect_err("builtin protected");
    assert!(matches!(protected, StoreError::InvalidInput(_)));
    store.delete_subagent_role(custom.id).expect("delete");
    let gone = store.get_subagent_role(custom.id).expect_err("gone");
    assert!(matches!(gone, StoreError::NotFound("subagent role")));
}

#[test]
fn subagent_runs_are_owned_and_lifecycle_bounded() {
    let (_directory, store) = adopted_store();
    let role = store
        .create_subagent_role(&NewSubagentRole {
            name: "researcher".to_string(),
            ..NewSubagentRole::default()
        })
        .expect("role");
    let run_id = seed_run(&store, Some(role.id), "investigate");

    let run = store
        .get_subagent_run("local-user", run_id)
        .expect("get run");
    assert_eq!(run.status, "queued");
    assert_eq!(run.parent_conversation_id, 1);
    assert!(run.started_at.len() >= 10);
    assert_eq!(run.finished_at, None);

    let listed = store
        .list_subagent_runs(
            "local-user",
            &SubagentRunFilter {
                status: Some("queued".to_string()),
                ..SubagentRunFilter::default()
            },
        )
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, run_id);
    assert!(
        store
            .list_subagent_runs(
                "local-user",
                &SubagentRunFilter {
                    status: Some("completed".to_string()),
                    ..SubagentRunFilter::default()
                },
            )
            .expect("filter")
            .is_empty()
    );

    let running = store
        .finish_subagent_run("local-user", run_id, "running", None, None, None)
        .expect_err("non terminal");
    assert!(matches!(running, StoreError::InvalidInput(_)));

    let finished = store
        .finish_subagent_run(
            "local-user",
            run_id,
            "completed",
            Some("all done"),
            Some(&json!({"total_tokens": 11})),
            None,
        )
        .expect("finish");
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.result_summary.as_deref(), Some("all done"));
    assert_eq!(finished.usage, Some(json!({"total_tokens": 11})));
    assert!(finished.finished_at.is_some());

    store
        .delete_subagent_run("local-user", run_id)
        .expect("delete terminal");
    let gone = store
        .get_subagent_run("local-user", run_id)
        .expect_err("gone");
    assert!(matches!(gone, StoreError::NotFound("subagent run")));
}

#[test]
fn non_terminal_runs_cannot_be_deleted() {
    let (_directory, store) = adopted_store();
    let run_id = seed_run(&store, None, "keep");
    let error = store
        .delete_subagent_run("local-user", run_id)
        .expect_err("queued run");
    assert!(matches!(error, StoreError::InvalidInput(_)));
    assert!(store.get_subagent_run("local-user", run_id).is_ok());
}

#[test]
fn subagent_runs_respect_actor_ownership() {
    let (_directory, store) = adopted_store();
    let run_id = seed_run(&store, None, "private");
    store.ensure_actor("other-actor").expect("register actor");

    assert!(
        store
            .list_subagent_runs("other-actor", &SubagentRunFilter::default())
            .expect("list")
            .is_empty()
    );
    let error = store
        .get_subagent_run("other-actor", run_id)
        .expect_err("cross actor");
    assert!(matches!(error, StoreError::NotFound("conversation")));
}

#[test]
fn subagent_creation_validates_parent_ownership() {
    let (_directory, store) = adopted_store();
    let isolated = store
        .create_conversation(
            "local-user",
            &NewConversation {
                metadata: Some(json!({"is_subagent": true})),
                ..NewConversation::default()
            },
        )
        .expect("isolated");
    store
        .update_conversation(
            "local-user",
            1,
            &ConversationPatch {
                title: Some("Parent".to_string()),
                ..ConversationPatch::default()
            },
        )
        .expect("touch parent");

    store.ensure_actor("other-actor").expect("register actor");
    let error = store
        .create_subagent_run(
            "other-actor",
            1,
            &NewSubagentRun {
                conversation_id: isolated.id,
                prompt: "nope".to_string(),
                ..NewSubagentRun::default()
            },
        )
        .expect_err("foreign parent");
    assert!(matches!(error, StoreError::NotFound("conversation")));
}
