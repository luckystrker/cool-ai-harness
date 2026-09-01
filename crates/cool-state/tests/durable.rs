use std::sync::{Arc, Barrier};

use cool_protocol::{
    ActorKind, ActorRef, ApprovalDecision, CanonicalEvent, EventEnvelope, RunStarted, RunTerminal,
    TextDelta, V1Version,
};
use cool_state::{
    ArtifactReference, BudgetDelta, BudgetLimits, DurableStore, EventProvenance, RunStatus,
    StoreError,
};
use tempfile::tempdir;

fn actor() -> ActorRef {
    ActorRef {
        id: "local-user".to_owned(),
        kind: ActorKind::LocalUser,
    }
}

fn event(session_id: &str, run_id: &str, seq: u64, event: CanonicalEvent) -> EventEnvelope {
    EventEnvelope {
        event_id: format!("event-{seq}"),
        schema_version: V1Version::VALUE,
        session_id: session_id.to_owned(),
        run_id: run_id.to_owned(),
        item_id: None,
        seq,
        occurred_at: "2026-09-01T00:00:00Z".to_owned(),
        actor: actor(),
        source: "test".to_owned(),
        causation_id: None,
        correlation_id: None,
        event,
        extensions: Default::default(),
    }
}

fn session_and_run(store: &DurableStore) -> (String, String) {
    let session = store
        .create_session("local-user", "session-key", "a", None, None)
        .unwrap()
        .value;
    let run = store
        .start_run("local-user", "run-key", "b", &session)
        .unwrap()
        .value;
    (session, run)
}

#[test]
fn invalid_transition_is_rejected_without_an_event_side_effect() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                1,
                CanonicalEvent::RunCompleted(RunTerminal {
                    reason: "done".to_owned(),
                    error_code: None,
                }),
            ),
        )
        .unwrap();
    let result = store.append_event(
        "local-user",
        &event(
            &session,
            &run,
            2,
            CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: None,
            }),
        ),
    );
    assert!(matches!(result, Err(StoreError::InvalidTransition { .. })));
    assert_eq!(store.all_events(&run, "local-user").unwrap().len(), 1);
}

#[test]
fn core_restart_recovers_one_unambiguous_terminal_state() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.db");
    let (session, run) = {
        let store = DurableStore::open(&path).unwrap();
        let (session, run) = session_and_run(&store);
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    1,
                    CanonicalEvent::RunStarted(RunStarted {
                        model: None,
                        mode: Some("test".to_owned()),
                    }),
                ),
            )
            .unwrap();
        (session, run)
    };
    let reopened = DurableStore::open(&path).unwrap();
    let recovered = reopened.recover_incomplete_runs().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].session_id, session);
    assert_eq!(
        reopened.replay_run(&run, "local-user").unwrap().status,
        RunStatus::Failed
    );
    assert!(reopened.recover_incomplete_runs().unwrap().is_empty());
}

#[test]
fn accepted_cancel_is_terminal_before_restart_and_preserves_its_reason() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("cancel-recovery.db");
    let run = {
        let store = DurableStore::open(&path).unwrap();
        let (_session, run) = session_and_run(&store);
        store
            .accept_cancel(
                "local-user",
                "cancel-before-crash",
                "cancel-fingerprint",
                &run,
                "user_requested",
                EventProvenance {
                    actor: actor(),
                    source: "test".to_owned(),
                },
            )
            .unwrap();
        run
    };
    let reopened = DurableStore::open(&path).unwrap();
    let recovered = reopened.recover_incomplete_runs().unwrap();
    assert!(recovered.is_empty());
    let events = reopened.all_events(&run, "local-user").unwrap();
    match &events[0].event {
        CanonicalEvent::RunCancelled(terminal) => {
            assert_eq!(terminal.reason, "user_requested");
        }
        event => panic!("expected recovered cancellation, got {event:?}"),
    }
    assert_eq!(
        reopened.replay_run(&run, "local-user").unwrap().status,
        RunStatus::Cancelled
    );
}

#[test]
fn accepted_cancel_and_completion_cannot_both_win() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let barrier = Arc::new(Barrier::new(3));
    let cancel_store = store.clone();
    let cancel_run = run.clone();
    let cancel_barrier = barrier.clone();
    let cancel = std::thread::spawn(move || {
        cancel_barrier.wait();
        cancel_store.accept_cancel(
            "local-user",
            "racing-cancel",
            "cancel-fingerprint",
            &cancel_run,
            "race",
            EventProvenance {
                actor: actor(),
                source: "test".to_owned(),
            },
        )
    });
    let complete_store = store.clone();
    let complete_run = run.clone();
    let complete_barrier = barrier.clone();
    let complete = std::thread::spawn(move || {
        complete_barrier.wait();
        complete_store.append_event_auto(
            "local-user",
            event(
                &session,
                &complete_run,
                0,
                CanonicalEvent::RunCompleted(RunTerminal {
                    reason: "race".to_owned(),
                    error_code: None,
                }),
            ),
        )
    });
    barrier.wait();
    let cancel = cancel.join().unwrap();
    let complete = complete.join().unwrap();
    assert_ne!(cancel.is_ok(), complete.is_ok());
    let replay = store.replay_run(&run, "local-user").unwrap();
    if cancel.is_ok() {
        assert_eq!(replay.status, RunStatus::Cancelled);
    } else {
        assert_eq!(replay.status, RunStatus::Completed);
    }
}

#[test]
fn one_approval_resolution_wins_the_race_and_is_audited_with_an_event() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let ticket = store
        .create_approval(
            "local-user",
            &session,
            &run,
            "call-1",
            "write_file",
            "write requires review",
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles = [ApprovalDecision::Approved, ApprovalDecision::Denied]
        .into_iter()
        .enumerate()
        .map(|(index, decision)| {
            let store = store.clone();
            let barrier = barrier.clone();
            let approval_id = ticket.approval_id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.resolve_approval(
                    "local-user",
                    &format!("resolve-{index}"),
                    &format!("fingerprint-{index}"),
                    &approval_id,
                    1,
                    decision,
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(store.all_events(&run, "local-user").unwrap().len(), 2);
    store.replay_run(&run, "local-user").unwrap();
}

#[test]
fn budget_check_and_increment_is_atomic_under_contention() {
    let store = DurableStore::in_memory().unwrap();
    store
        .set_budget_limits(
            "local-user",
            "daily:2026-09-01",
            BudgetLimits {
                iterations: Some(10),
                ..BudgetLimits::default()
            },
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(21));
    let handles = (0..20)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.reserve_budget(
                    "local-user",
                    "daily:2026-09-01",
                    BudgetDelta {
                        iterations: 1,
                        ..BudgetDelta::default()
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 10);
    assert!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::BudgetExceeded(_))))
            .count()
            == 10
    );
}

#[test]
fn idempotency_is_actor_scoped_and_rejects_changed_inputs() {
    let store = DurableStore::in_memory().unwrap();
    let first = store
        .create_session("local-user", "same", "one", Some("A"), None)
        .unwrap();
    let replay = store
        .create_session("local-user", "same", "one", Some("A"), None)
        .unwrap();
    assert_eq!(first.value, replay.value);
    assert!(!replay.created);
    assert!(matches!(
        store.create_session("local-user", "same", "two", Some("B"), None),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(
        store
            .create_session("another-user", "same", "two", Some("B"), None)
            .is_ok()
    );
}

#[test]
fn namespaced_migration_preserves_existing_python_tables() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("mixed.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE conversations(id INTEGER PRIMARY KEY, title TEXT);\
                 INSERT INTO conversations(id, title) VALUES (1, 'keep me');",
            )
            .unwrap();
    }
    let store = DurableStore::open(&path).unwrap();
    store
        .create_session("local-user", "key", "fingerprint", None, None)
        .unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&path).unwrap();
    let title: String = connection
        .query_row("SELECT title FROM conversations WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(title, "keep me");
}

#[test]
fn newer_schema_version_fails_closed_without_downgrade() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("future.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE rust_schema_meta(version INTEGER NOT NULL); INSERT INTO rust_schema_meta VALUES (99);")
            .unwrap();
    }
    assert!(matches!(
        DurableStore::open(&path),
        Err(StoreError::Corrupt(_))
    ));
    let connection = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = connection
        .query_row("SELECT version FROM rust_schema_meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 99);
}

#[test]
fn concurrent_auto_append_allocates_every_sequence_once() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let barrier = Arc::new(Barrier::new(21));
    let handles = (0..20)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            let mut envelope = event(
                &session,
                &run,
                0,
                CanonicalEvent::ContentDelta(TextDelta {
                    text: index.to_string(),
                    channel: None,
                }),
            );
            envelope.event_id = format!("concurrent-event-{index}");
            std::thread::spawn(move || {
                barrier.wait();
                store.append_event_auto("local-user", envelope).unwrap().seq
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let mut sequences = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=20).collect::<Vec<_>>());
}

#[test]
fn concurrent_cancel_retry_is_recorded_once_before_signalling() {
    let store = DurableStore::in_memory().unwrap();
    let (_session, run) = session_and_run(&store);
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            let run = run.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .accept_cancel(
                        "local-user",
                        "cancel",
                        "same",
                        &run,
                        "user",
                        EventProvenance {
                            actor: actor(),
                            source: "test".to_owned(),
                        },
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.created).count(), 1);
    assert!(outcomes.iter().all(|outcome| outcome.result.accepted));
}

#[test]
fn artifact_reference_is_actor_bound_content_addressed_and_relative() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let valid = ArtifactReference {
        artifact_id: "artifact-1".to_owned(),
        session_id: session.clone(),
        run_id: Some(run.clone()),
        sha256: "a".repeat(64),
        size_bytes: 12,
        storage_path: "aa/content".to_owned(),
        actor_id: "local-user".to_owned(),
        source: "tool:write_file".to_owned(),
    };
    store.add_artifact_reference(&valid).unwrap();

    let mut escaping = valid.clone();
    escaping.artifact_id = "artifact-2".to_owned();
    escaping.storage_path = "../outside".to_owned();
    assert!(matches!(
        store.add_artifact_reference(&escaping),
        Err(StoreError::Corrupt(_))
    ));

    let mut wrong_actor = valid;
    wrong_actor.artifact_id = "artifact-3".to_owned();
    wrong_actor.actor_id = "another-user".to_owned();
    assert!(matches!(
        store.add_artifact_reference(&wrong_actor),
        Err(StoreError::ActorMismatch)
    ));
}
