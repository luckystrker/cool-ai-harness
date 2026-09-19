//! Run/event/tool-call/approval store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::runs::{NewApprovalAudit, NewRun, NewToolCall, RunFilter};
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
fn runs_and_events_carry_monotonic_sequences_and_terminal_state() {
    let (_directory, store) = adopted_store();
    let run = store
        .create_run(
            "local-user",
            1,
            &NewRun {
                model: Some("test-model".to_string()),
                config: Some(json!({"temperature": 0})),
            },
        )
        .expect("create");
    assert_eq!(run.status, "running");

    let first = store
        .append_run_event("local-user", run.id, "run.started", None)
        .expect("event 1");
    let second = store
        .append_run_event(
            "local-user",
            run.id,
            "content.delta",
            Some(&json!({"text": "hi"})),
        )
        .expect("event 2");
    // Python allocates the first run event at seq 0 (`app/agent/service.py`).
    assert_eq!((first.seq, second.seq), (0, 1));

    let events = store
        .list_run_events("local-user", run.id, Some(0), None)
        .expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "content.delta");

    let finished = store
        .finish_run(
            "local-user",
            run.id,
            "completed",
            Some(&json!({"total_tokens": 9})),
            Some(1),
            Some("stop"),
            None,
        )
        .expect("finish");
    assert_eq!(finished.status, "completed");
    assert!(finished.finished_at.is_some());
    assert_eq!(
        store
            .list_runs("local-user", 1, &RunFilter::default())
            .expect("list")
            .len(),
        1
    );

    let error = store
        .record_approval_audit(
            "local-user",
            1,
            &NewApprovalAudit {
                run_id: Some(run.id),
                call_id: "call-1".to_string(),
                tool_name: "write_file".to_string(),
                arguments: Some(json!({"path": "a.txt"})),
                approved: true,
                decision_source: "user".to_string(),
                decided_by: Some("local-user".to_string()),
                reason: None,
                is_breakpoint: false,
                breakpoint_type: None,
                duration_ms: Some(12),
            },
        )
        .expect("audit ok");
    assert!(error.approved);

    let audit_error = store
        .record_approval_audit(
            "local-user",
            999,
            &NewApprovalAudit {
                run_id: None,
                call_id: "call-2".to_string(),
                tool_name: "write_file".to_string(),
                arguments: None,
                approved: false,
                decision_source: "policy".to_string(),
                decided_by: None,
                reason: Some("denied".to_string()),
                is_breakpoint: false,
                breakpoint_type: None,
                duration_ms: None,
            },
        )
        .expect_err("unknown conversation");
    assert!(matches!(audit_error, StoreError::NotFound("conversation")));

    let audits = store
        .list_approval_audits("local-user", 1, Some(run.id), None)
        .expect("list audits");
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].call_id, "call-1");
}

#[test]
fn tool_calls_are_actor_scoped_and_ordered_newest_first() {
    let (_directory, store) = adopted_store();
    for name in ["read_file", "write_file"] {
        store
            .record_tool_call(
                "local-user",
                &NewToolCall {
                    conversation_id: Some(1),
                    message_id: None,
                    name: name.to_string(),
                    arguments: Some(json!({"path": "x"})),
                    result: Some(json!({"ok": true})),
                    duration_ms: Some(5),
                    success: true,
                    error: None,
                },
            )
            .expect("record");
    }
    let calls = store
        .list_tool_calls("local-user", Some(1), None)
        .expect("list");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "write_file");

    store.ensure_actor("other-actor").expect("register");
    store
        .record_tool_call(
            "other-actor",
            &NewToolCall {
                conversation_id: None,
                message_id: None,
                name: "read_file".to_string(),
                arguments: None,
                result: None,
                duration_ms: None,
                success: false,
                error: Some("boom".to_string()),
            },
        )
        .expect("other actor");
    let own = store
        .list_tool_calls("local-user", None, None)
        .expect("own");
    assert_eq!(own.len(), 2);
}

#[test]
fn tool_call_reads_are_actor_scoped_and_null_owner_rows_are_hidden() {
    let (_directory, store) = adopted_store();
    let own = store
        .record_tool_call(
            "local-user",
            &NewToolCall {
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
        .expect("record");
    store.ensure_actor("other-actor").expect("register");

    assert_eq!(
        store
            .get_tool_call("local-user", own.id)
            .expect("owner reads")
            .id,
        own.id
    );
    let error = store
        .get_tool_call("other-actor", own.id)
        .expect_err("cross-actor read must fail");
    assert!(matches!(error, StoreError::NotFound(_)));

    // Legacy service-level rows without any owner are not readable by anyone.
    let orphan_id = store
        .with_connection(|connection| {
            connection.execute(
                "INSERT INTO tool_calls(created_at, updated_at, conversation_id, message_id,
                   user_id, name, success) VALUES ('2026-01-01 00:00:00.000000',
                   '2026-01-01 00:00:00.000000', NULL, NULL, NULL, 'legacy', 1)",
                [],
            )?;
            Ok(connection.last_insert_rowid())
        })
        .expect("orphan");
    let error = store
        .get_tool_call("local-user", orphan_id)
        .expect_err("null-owner row must stay hidden");
    assert!(matches!(error, StoreError::NotFound("tool call")));
}

#[test]
fn run_reads_respect_actor_ownership() {
    let (_directory, store) = adopted_store();
    let run = store
        .create_run("local-user", 1, &NewRun::default())
        .expect("create");
    store.ensure_actor("other-actor").expect("register actor");
    store
        .create_conversation("other-actor", &Default::default())
        .expect("other conversation");
    let error = store
        .get_run("other-actor", run.id)
        .expect_err("cross-actor run read must fail");
    assert!(matches!(
        error,
        StoreError::NotFound("run") | StoreError::NotFound("conversation")
    ));
}
