use std::sync::{Arc, Barrier};

use cool_protocol::{
    ActorKind, ActorRef, ApprovalDecision, CanonicalEvent, EventEnvelope, ItemEvent, RunStarted,
    RunTerminal, TextDelta, ToolRequested, V1Version,
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
        event_id: format!("{run_id}-event-{seq}"),
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
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    2,
                    CanonicalEvent::ItemCompleted(ItemEvent {
                        role: Some("assistant".to_owned()),
                        content: None,
                        tool_calls: vec![ToolRequested {
                            call_id: "interrupted-call".to_owned(),
                            name: "write_file".to_owned(),
                            arguments: Default::default(),
                        }],
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
    let events = reopened.all_events(&run, "local-user").unwrap();
    assert!(matches!(events[2].event, CanonicalEvent::ToolFailed(_)));
    assert!(matches!(events[3].event, CanonicalEvent::RunFailed(_)));
    assert!(reopened.recover_incomplete_runs().unwrap().is_empty());
}

#[test]
fn accepted_cancel_closes_pending_tool_calls_before_the_terminal_event() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                1,
                CanonicalEvent::ItemCompleted(ItemEvent {
                    role: Some("assistant".to_owned()),
                    content: None,
                    tool_calls: vec![ToolRequested {
                        call_id: "pending-call".to_owned(),
                        name: "shell".to_owned(),
                        arguments: Default::default(),
                    }],
                }),
            ),
        )
        .unwrap();
    store
        .accept_cancel(
            "local-user",
            "cancel-open-batch",
            "cancel-open-batch-fingerprint",
            &run,
            "user_requested",
            EventProvenance {
                actor: actor(),
                source: "test".to_owned(),
            },
        )
        .unwrap();
    let events = store.all_events(&run, "local-user").unwrap();
    assert!(matches!(events[1].event, CanonicalEvent::ToolFailed(_)));
    assert!(matches!(events[2].event, CanonicalEvent::RunCancelled(_)));
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
                    None,
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
    let winner = results
        .iter()
        .find_map(|result| result.as_ref().ok())
        .unwrap();
    assert_eq!(
        store
            .approval_outcome("local-user", &ticket.approval_id)
            .unwrap(),
        Some((winner.outcome.clone(), None))
    );
    assert_eq!(store.all_events(&run, "local-user").unwrap().len(), 2);
    store.replay_run(&run, "local-user").unwrap();
}

#[test]
fn expire_approval_times_out_the_ticket_and_unblocks_the_run() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let ticket = store
        .create_approval(
            "local-user",
            &session,
            &run,
            "call-1",
            "ask_user",
            "which credential?",
        )
        .unwrap();
    assert_eq!(
        store.replay_run(&run, "local-user").unwrap().status,
        RunStatus::AwaitingApproval
    );

    assert!(
        store
            .expire_approval("local-user", &ticket.approval_id)
            .unwrap()
    );
    // A second expiry — or a late user answer — loses to the first
    // resolution instead of erroring or rewriting state.
    assert!(
        !store
            .expire_approval("local-user", &ticket.approval_id)
            .unwrap()
    );
    assert!(
        store
            .resolve_approval(
                "local-user",
                "late-resolve",
                "fingerprint-late",
                &ticket.approval_id,
                1,
                ApprovalDecision::Approved,
                None,
            )
            .is_err()
    );
    assert_eq!(
        store
            .approval_outcome("local-user", &ticket.approval_id)
            .unwrap(),
        Some((cool_protocol::ApprovalOutcome::TimedOut, None))
    );
    assert_eq!(
        store.replay_run(&run, "local-user").unwrap().status,
        RunStatus::Running,
        "ToolApprovalResolved flips the run back to running"
    );
}

#[test]
fn resolved_question_answers_persist_masked() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let ticket = store
        .create_approval(
            "local-user",
            &session,
            &run,
            "call-1",
            "ask_user",
            "enter the token",
        )
        .unwrap();
    let secret = "token = abcdefgh12345678";
    let resolution = store
        .resolve_approval(
            "local-user",
            "resolve-key",
            "fingerprint-key",
            &ticket.approval_id,
            1,
            ApprovalDecision::Approved,
            Some(&serde_json::json!(secret)),
        )
        .unwrap();
    // The in-memory resolution keeps the raw answer for the waiting run…
    assert_eq!(resolution.answer.as_ref(), Some(&serde_json::json!(secret)));
    // …but the persisted copy is masked.
    let persisted = store
        .approval_outcome("local-user", &ticket.approval_id)
        .unwrap()
        .expect("resolved outcome")
        .1
        .expect("persisted answer");
    assert_ne!(persisted, serde_json::json!(secret));
    assert!(
        persisted
            .as_str()
            .unwrap_or_default()
            .contains("[REDACTED]"),
        "persisted answer is masked: {persisted}"
    );
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
fn unknown_cost_fails_closed_when_a_cost_ceiling_is_configured() {
    let store = DurableStore::in_memory().unwrap();
    store
        .set_budget_limits(
            "local-user",
            "cost-limited",
            BudgetLimits {
                cost_microusd: Some(10),
                ..BudgetLimits::default()
            },
        )
        .unwrap();
    assert!(matches!(
        store.reserve_budget(
            "local-user",
            "cost-limited",
            BudgetDelta {
                tokens: 1,
                cost_microusd: None,
                iterations: 1,
                proactive_actions: 0,
            }
        ),
        Err(StoreError::BudgetExceeded(_))
    ));
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

#[test]
fn session_history_is_ordered_across_runs_from_the_canonical_event_log() {
    let store = DurableStore::in_memory().unwrap();
    let session = store
        .create_session("local-user", "session", "session", None, None)
        .unwrap()
        .value;
    for index in 0..2 {
        let run = store
            .start_run(
                "local-user",
                &format!("run-{index}"),
                &format!("run-{index}"),
                &session,
            )
            .unwrap()
            .value;
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    1,
                    CanonicalEvent::RunStarted(RunStarted {
                        model: None,
                        mode: None,
                    }),
                ),
            )
            .unwrap();
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    2,
                    CanonicalEvent::ItemCompleted(ItemEvent {
                        role: Some("user".to_owned()),
                        content: Some(format!("prompt-{index}")),
                        tool_calls: Vec::new(),
                    }),
                ),
            )
            .unwrap();
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    3,
                    CanonicalEvent::RunCompleted(RunTerminal {
                        reason: "stop".to_owned(),
                        error_code: None,
                    }),
                ),
            )
            .unwrap();
    }
    let events = store.session_events(&session, "local-user").unwrap();
    assert_eq!(events.len(), 6);
    let prompts = events
        .iter()
        .filter_map(|event| match &event.event {
            CanonicalEvent::ItemCompleted(item) => item.content.as_deref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["prompt-0", "prompt-1"]);
    assert!(matches!(
        store.session_events(&session, "another-user"),
        Err(StoreError::ActorMismatch)
    ));
}

#[test]
fn durable_writer_masks_secrets_for_direct_event_callers() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                1,
                CanonicalEvent::ItemCompleted(ItemEvent {
                    role: Some("user".to_owned()),
                    content: Some(
                        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456".to_owned(),
                    ),
                    tool_calls: vec![ToolRequested {
                        call_id: "masked-call".to_owned(),
                        name: "example".to_owned(),
                        arguments: [("api_key".to_owned(), serde_json::json!("short-secret"))]
                            .into_iter()
                            .collect(),
                    }],
                }),
            ),
        )
        .unwrap();
    let encoded = serde_json::to_string(&store.all_events(&run, "local-user").unwrap()).unwrap();
    assert!(!encoded.contains("abcdefghijklmnopqrstuvwxyz123456"));
    assert!(!encoded.contains("short-secret"));
    assert!(encoded.contains("[REDACTED]"));
}

#[test]
fn session_listing_is_actor_scoped_filtered_and_bounded() {
    let store = DurableStore::in_memory().unwrap();
    for index in 0..3 {
        store
            .create_session(
                "local-user",
                &format!("key-{index}"),
                &format!("key-{index}"),
                Some(&format!("session {index}")),
                Some(if index == 0 { "project-a" } else { "project-b" }),
            )
            .unwrap();
    }
    store
        .create_session("another-user", "other", "other", None, Some("project-a"))
        .unwrap();

    let all = store.list_sessions("local-user", None, 10).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].title.as_deref(), Some("session 2"));
    assert_eq!(all[0].created_at.len(), "2026-01-01T00:00:00.000Z".len());

    let filtered = store
        .list_sessions("local-user", Some("project-a"), 10)
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].project_key.as_deref(), Some("project-a"));

    let bounded = store.list_sessions("local-user", None, 2).unwrap();
    assert_eq!(bounded.len(), 2);
    assert!(store.list_sessions("another-user", None, 10).unwrap().len() == 1);
}

#[test]
fn fork_copies_history_into_a_new_session_without_mutating_the_source() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    for (seq, canonical) in [
        (
            1,
            CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: None,
            }),
        ),
        (
            2,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("user".to_owned()),
                content: Some("original prompt".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            3,
            CanonicalEvent::ToolCompleted(cool_protocol::ToolCompleted {
                call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                result: serde_json::json!({"content": "hello"}),
            }),
        ),
        (
            4,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: Some("original answer".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            5,
            CanonicalEvent::RunCompleted(RunTerminal {
                reason: "stop".to_owned(),
                error_code: None,
            }),
        ),
    ] {
        store
            .append_event("local-user", &event(&session, &run, seq, canonical))
            .unwrap();
    }

    let forked = store
        .fork_session(
            "local-user",
            "fork-key",
            "fork-a",
            &session,
            Some("branch"),
            None,
            None,
        )
        .unwrap();
    assert!(forked.created);
    assert_ne!(forked.value, session);

    let forked_events = store.session_events(&forked.value, "local-user").unwrap();
    assert_eq!(forked_events.len(), 5);
    assert!(forked_events.iter().all(|event| event.run_id != run));
    assert_eq!(forked_events[0].seq, 1);
    assert!(matches!(
        forked_events[0].event,
        CanonicalEvent::RunStarted(_)
    ));
    assert!(matches!(
        forked_events.last().unwrap().event,
        CanonicalEvent::RunCompleted(_)
    ));
    let original = store
        .session_events(&session, "local-user")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            CanonicalEvent::ItemCompleted(item) => item.content,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(original, ["original prompt", "original answer"]);
    let copied = forked_events
        .iter()
        .filter_map(|event| match &event.event {
            CanonicalEvent::ItemCompleted(item) => item.content.clone(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(copied, ["original prompt", "original answer"]);
    assert!(forked_events.iter().any(|event| matches!(
        &event.event,
        CanonicalEvent::ToolCompleted(tool) if tool.call_id == "call-1"
    )));

    let replay = store
        .fork_session(
            "local-user",
            "fork-key",
            "fork-a",
            &session,
            Some("branch"),
            None,
            None,
        )
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.value, forked.value);
    assert!(matches!(
        store.fork_session(
            "local-user",
            "fork-key",
            "different",
            &session,
            None,
            None,
            None,
        ),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(matches!(
        store.fork_session(
            "another-user",
            "fork-other",
            "x",
            &session,
            None,
            None,
            None,
        ),
        Err(StoreError::ActorMismatch)
    ));

    let new_run = store
        .start_run("local-user", "after-fork", "after-fork", &forked.value)
        .unwrap();
    assert!(new_run.created);
}

#[test]
fn fork_preserves_multi_run_history_order_not_per_run_sequences() {
    let store = DurableStore::in_memory().unwrap();
    let session = store
        .create_session("local-user", "multi-session", "multi-session", None, None)
        .unwrap()
        .value;
    for (index, answer) in ["answer-1", "answer-2"].iter().enumerate() {
        let run = store
            .start_run(
                "local-user",
                &format!("multi-run-{index}"),
                &format!("multi-run-{index}"),
                &session,
            )
            .unwrap()
            .value;
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    1,
                    CanonicalEvent::RunStarted(RunStarted {
                        model: None,
                        mode: None,
                    }),
                ),
            )
            .unwrap();
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    2,
                    CanonicalEvent::ItemCompleted(ItemEvent {
                        role: Some("user".to_owned()),
                        content: Some(format!("prompt-{index}")),
                        tool_calls: Vec::new(),
                    }),
                ),
            )
            .unwrap();
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    3,
                    CanonicalEvent::ItemCompleted(ItemEvent {
                        role: Some("assistant".to_owned()),
                        content: Some((*answer).to_owned()),
                        tool_calls: Vec::new(),
                    }),
                ),
            )
            .unwrap();
        store
            .append_event(
                "local-user",
                &event(
                    &session,
                    &run,
                    4,
                    CanonicalEvent::RunCompleted(RunTerminal {
                        reason: "stop".to_owned(),
                        error_code: None,
                    }),
                ),
            )
            .unwrap();
    }

    let forked = store
        .fork_session(
            "local-user",
            "multi-fork",
            "multi-fork",
            &session,
            None,
            None,
            None,
        )
        .unwrap()
        .value;
    let contents = store
        .session_events(&forked, "local-user")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            CanonicalEvent::ItemCompleted(item) => item.content,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contents, ["prompt-0", "answer-1", "prompt-1", "answer-2"]);
}

#[test]
fn steer_appends_a_durable_user_item_only_to_active_runs() {
    let store = DurableStore::in_memory().unwrap();
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
                    mode: None,
                }),
            ),
        )
        .unwrap();

    let steer = store
        .steer_run(
            "local-user",
            "steer-key",
            "steer-fingerprint",
            &run,
            "focus",
        )
        .unwrap();
    assert!(steer.created);
    assert_eq!(steer.value.run_id, run);
    assert_eq!(steer.value.seq, 2);

    let events = store.all_events(&run, "local-user").unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[1].event,
        CanonicalEvent::ItemCompleted(item)
            if item.role.as_deref() == Some("user") && item.content.as_deref() == Some("focus")
    ));

    let replay = store
        .steer_run(
            "local-user",
            "steer-key",
            "steer-fingerprint",
            &run,
            "focus",
        )
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.value.seq, 2);
    assert!(matches!(
        store.steer_run("local-user", "steer-key", "changed", &run, "focus"),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(matches!(
        store.steer_run("another-user", "foreign", "foreign", &run, "focus"),
        Err(StoreError::ActorMismatch)
    ));

    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                3,
                CanonicalEvent::RunCompleted(RunTerminal {
                    reason: "stop".to_owned(),
                    error_code: None,
                }),
            ),
        )
        .unwrap();
    assert!(matches!(
        store.steer_run("local-user", "late", "late", &run, "too late"),
        Err(StoreError::RunNotActive)
    ));
}

#[test]
fn fork_filters_history_at_cursor() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    for (seq, canonical) in [
        (
            1,
            CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: None,
            }),
        ),
        (
            2,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("user".to_owned()),
                content: Some("prompt-0".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            3,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: Some("answer-1".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            4,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("user".to_owned()),
                content: Some("prompt-1".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            5,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: Some("answer-2".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            6,
            CanonicalEvent::RunCompleted(RunTerminal {
                reason: "stop".to_owned(),
                error_code: None,
            }),
        ),
    ] {
        store
            .append_event("local-user", &event(&session, &run, seq, canonical))
            .unwrap();
    }

    // The cursor is `rust_events.rowid`, projected by the history window —
    // the same value the UI holds on each message.
    let answer_cursor = store
        .session_event_window(&session, "local-user", None, usize::MAX)
        .unwrap()
        .into_iter()
        .find(|(_, envelope)| {
            matches!(
                &envelope.event,
                CanonicalEvent::ItemCompleted(item) if item.content.as_deref() == Some("answer-1")
            )
        })
        .map(|(cursor, _)| cursor)
        .unwrap();

    let forked = store
        .fork_session(
            "local-user",
            "fork-cursor",
            "fork-cursor",
            &session,
            None,
            Some(answer_cursor),
            None,
        )
        .unwrap()
        .value;
    let contents = store
        .session_events(&forked, "local-user")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            CanonicalEvent::ItemCompleted(item) => item.content,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contents, ["prompt-0", "answer-1"]);

    // `up_to_event_seq` bounds the per-run seq space instead — seq <= 3 keeps
    // the same prefix here, but only when the cursor bound is absent.
    let seq_bounded = store
        .fork_session(
            "local-user",
            "fork-seq",
            "fork-seq",
            &session,
            None,
            None,
            Some(3),
        )
        .unwrap()
        .value;
    let seq_contents = store
        .session_events(&seq_bounded, "local-user")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            CanonicalEvent::ItemCompleted(item) => item.content,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(seq_contents, ["prompt-0", "answer-1"]);

    // A cursor inside the run drops everything past it — even the run's own
    // terminal marker — but the source session is untouched.
    let prompt_cursor = store
        .session_event_window(&session, "local-user", None, usize::MAX)
        .unwrap()
        .into_iter()
        .find(|(_, envelope)| {
            matches!(
                &envelope.event,
                CanonicalEvent::ItemCompleted(item) if item.content.as_deref() == Some("prompt-0")
            )
        })
        .map(|(cursor, _)| cursor)
        .unwrap();
    let shallow = store
        .fork_session(
            "local-user",
            "fork-shallow",
            "fork-shallow",
            &session,
            None,
            Some(prompt_cursor),
            None,
        )
        .unwrap()
        .value;
    let shallow_contents = store
        .session_events(&shallow, "local-user")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.event {
            CanonicalEvent::ItemCompleted(item) => item.content,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(shallow_contents, ["prompt-0"]);
    assert_eq!(
        store.session_events(&session, "local-user").unwrap().len(),
        6
    );
}

#[test]
fn rewind_marks_runs_and_replays_prefix() {
    let store = DurableStore::in_memory().unwrap();
    let (session, run) = session_and_run(&store);
    let mut checkpoint = event(
        &session,
        &run,
        4,
        CanonicalEvent::ToolStarted(cool_protocol::ToolLifecycle {
            call_id: "call-write".to_owned(),
            name: "write_file".to_owned(),
        }),
    );
    checkpoint.extensions.insert(
        "checkpoint_ref".to_owned(),
        serde_json::Value::String("refs/cool/checkpoints/s/9".to_owned()),
    );
    for (seq, canonical) in [
        (
            1,
            CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: None,
            }),
        ),
        (
            2,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("user".to_owned()),
                content: Some("prompt-0".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
        (
            3,
            CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: Some("answer-1".to_owned()),
                tool_calls: Vec::new(),
            }),
        ),
    ] {
        store
            .append_event("local-user", &event(&session, &run, seq, canonical))
            .unwrap();
    }
    store.append_event("local-user", &checkpoint).unwrap();
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                5,
                CanonicalEvent::ItemCompleted(ItemEvent {
                    role: Some("assistant".to_owned()),
                    content: Some("answer-2".to_owned()),
                    tool_calls: Vec::new(),
                }),
            ),
        )
        .unwrap();
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                6,
                CanonicalEvent::RunCompleted(RunTerminal {
                    reason: "stop".to_owned(),
                    error_code: None,
                }),
            ),
        )
        .unwrap();

    // Rewind to just after "answer-1" (cursor of that event).
    let answer_cursor = store
        .session_event_window(&session, "local-user", None, usize::MAX)
        .unwrap()
        .into_iter()
        .find(|(_, envelope)| {
            matches!(
                &envelope.event,
                CanonicalEvent::ItemCompleted(item) if item.content.as_deref() == Some("answer-1")
            )
        })
        .map(|(cursor, _)| cursor)
        .unwrap();

    let outcome = store
        .rewind_session(
            "local-user",
            "rewind-key",
            "rewind-fp",
            &session,
            answer_cursor,
            Some("bad turn"),
        )
        .unwrap();
    assert!(outcome.created);
    assert_eq!(outcome.value.rewound_run_ids, [run.clone()]);
    // The checkpoint lives past the cursor (seq 4 > cursor of seq 3) — it is
    // NOT picked up: refs are collected only inside the retained prefix.
    assert_eq!(outcome.value.checkpoint_ref, None);
    assert_eq!(
        store.run(&run, "local-user").unwrap().status,
        RunStatus::Rewound
    );

    // The seed run carries mode=rewind RunStarted + the retained prefix +
    // run.rewound + RunCompleted — and the session has no active run.
    let seed_events = store
        .all_events(&outcome.value.run_id, "local-user")
        .unwrap();
    assert!(matches!(
        &seed_events[0].event,
        CanonicalEvent::RunStarted(started) if started.mode.as_deref() == Some("rewind")
    ));
    let copied = seed_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            CanonicalEvent::ItemCompleted(item) => item.content.clone(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(copied, ["prompt-0", "answer-1"]);
    assert!(seed_events.iter().any(|envelope| matches!(
        &envelope.event,
        CanonicalEvent::RunRewound(rewound)
            if rewound.cursor == answer_cursor && rewound.reason.as_deref() == Some("bad turn")
    )));
    assert!(matches!(
        seed_events.last().unwrap().event,
        CanonicalEvent::RunCompleted(_)
    ));
    let snapshot = store.load_session(&session, "local-user").unwrap();
    assert_eq!(snapshot.active_run_id, None);

    // Append-only: the superseded run's events stay durable at run scope and
    // in the raw `session_events` log, while the history window only sees the
    // retained prefix replayed by the seed run.
    assert_eq!(store.all_events(&run, "local-user").unwrap().len(), 6);
    assert_eq!(
        store
            .session_events(&session, "local-user")
            .unwrap()
            .iter()
            .filter(|envelope| envelope.run_id == run)
            .count(),
        6
    );
    let visible = store
        .session_event_window(&session, "local-user", None, usize::MAX)
        .unwrap()
        .into_iter()
        .filter_map(|(_, envelope)| match envelope.event {
            CanonicalEvent::ItemCompleted(item) => item.content,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(visible, ["prompt-0", "answer-1"]);

    // Idempotent replay returns the same outcome; a foreign actor cannot rewind.
    let replay = store
        .rewind_session(
            "local-user",
            "rewind-key",
            "rewind-fp",
            &session,
            answer_cursor,
            Some("bad turn"),
        )
        .unwrap();
    assert!(!replay.created);
    assert_eq!(replay.value.run_id, outcome.value.run_id);
    assert!(matches!(
        store.rewind_session(
            "another-user",
            "other",
            "other",
            &session,
            answer_cursor,
            None,
        ),
        Err(StoreError::ActorMismatch)
    ));
}

#[test]
fn rewind_rejects_live_run_and_nothing_beyond_cursor() {
    let store = DurableStore::in_memory().unwrap();
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
                    mode: None,
                }),
            ),
        )
        .unwrap();
    assert!(matches!(
        store.rewind_session("local-user", "k1", "k1", &session, 0, None),
        Err(StoreError::RewindRejected("session_has_live_run"))
    ));
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                2,
                CanonicalEvent::ItemCompleted(ItemEvent {
                    role: Some("user".to_owned()),
                    content: Some("only".to_owned()),
                    tool_calls: Vec::new(),
                }),
            ),
        )
        .unwrap();
    store
        .append_event(
            "local-user",
            &event(
                &session,
                &run,
                3,
                CanonicalEvent::RunCompleted(RunTerminal {
                    reason: "stop".to_owned(),
                    error_code: None,
                }),
            ),
        )
        .unwrap();
    // Cursor at/past the last event: nothing is beyond it to rewind.
    assert!(matches!(
        store.rewind_session("local-user", "k2", "k2", &session, u64::MAX, None),
        Err(StoreError::RewindRejected("nothing_to_rewind"))
    ));
}
