//! Conversation -> durable session linking and history import (M11 cutover).

use std::sync::{Arc, Barrier};

use cool_protocol::{
    ActorKind, ActorRef, CanonicalEvent, EventEnvelope, ItemEvent, RunStarted, RunTerminal,
    TextDelta, ToolCompleted, ToolRequested, V1Version,
};
use cool_state::{DurableStore, ImportedHistoryEvent, RunStatus, StoreError};

fn history() -> Vec<ImportedHistoryEvent> {
    vec![
        ImportedHistoryEvent {
            occurred_at: "2026-09-01 10:00:00.000000".to_owned(),
            event: CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("user".to_owned()),
                content: Some("hello".to_owned()),
                tool_calls: Vec::new(),
            }),
        },
        ImportedHistoryEvent {
            occurred_at: "2026-09-01 10:00:01.000000".to_owned(),
            event: CanonicalEvent::ReasoningDelta(TextDelta {
                text: "thinking".to_owned(),
                channel: Some("analysis".to_owned()),
            }),
        },
        ImportedHistoryEvent {
            occurred_at: "2026-09-01 10:00:02.000000".to_owned(),
            event: CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: Some("calling a tool".to_owned()),
                tool_calls: vec![ToolRequested {
                    call_id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: Default::default(),
                }],
            }),
        },
        ImportedHistoryEvent {
            occurred_at: "2026-09-01 10:00:03.000000".to_owned(),
            event: CanonicalEvent::ToolCompleted(ToolCompleted {
                call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                result: serde_json::json!({"output": "contents"}),
            }),
        },
    ]
}

#[test]
fn link_imports_ordered_history_and_replays_without_duplication() {
    let store = DurableStore::in_memory().unwrap();
    let linked = store
        .link_conversation(
            "local-user",
            "link-1",
            "fingerprint-1",
            42,
            Some("Legacy chat"),
            Some("C:/work"),
            &history(),
            false,
        )
        .unwrap();
    assert!(linked.created);
    assert_eq!(linked.conversation_id, 42);
    assert_eq!(linked.imported_events, 4);

    let snapshot = store
        .load_session(&linked.session_id, "local-user")
        .unwrap();
    assert_eq!(snapshot.title.as_deref(), Some("Legacy chat"));
    assert_eq!(snapshot.project_key.as_deref(), Some("C:/work"));

    let events = store
        .session_events(&linked.session_id, "local-user")
        .unwrap();
    assert_eq!(events.len(), 6, "run.started + 4 items + run.completed");
    assert!(events.iter().enumerate().all(|(index, event)| {
        event.seq == index as u64 + 1 && event.session_id == linked.session_id
    }));
    assert!(matches!(events[0].event, CanonicalEvent::RunStarted(_)));
    assert!(matches!(events[1].event, CanonicalEvent::ItemCompleted(_)));
    assert!(matches!(events[2].event, CanonicalEvent::ReasoningDelta(_)));
    assert!(matches!(events[4].event, CanonicalEvent::ToolCompleted(_)));
    assert!(matches!(events[5].event, CanonicalEvent::RunCompleted(_)));

    let replay = store
        .link_conversation(
            "local-user",
            "link-1",
            "fingerprint-1",
            42,
            Some("Different"),
            None,
            &[],
            false,
        )
        .unwrap();
    assert_eq!(replay, linked, "same key replays the original outcome");

    let different_key = store
        .link_conversation(
            "local-user",
            "link-2",
            "fingerprint-2",
            42,
            None,
            None,
            &[],
            false,
        )
        .unwrap();
    assert!(!different_key.created);
    assert_eq!(different_key.session_id, linked.session_id);
    let events_after = store
        .session_events(&linked.session_id, "local-user")
        .unwrap();
    assert_eq!(events_after.len(), 6, "no second import run");

    let conflict = store.link_conversation(
        "local-user",
        "link-1",
        "different-fingerprint",
        42,
        None,
        None,
        &[],
        false,
    );
    assert!(matches!(conflict, Err(StoreError::IdempotencyConflict)));
}

#[test]
fn linking_and_run_listing_are_actor_scoped() {
    let store = DurableStore::in_memory().unwrap();
    let linked = store
        .link_conversation(
            "local-user",
            "link-1",
            "fingerprint",
            7,
            Some("Owned"),
            None,
            &history(),
            false,
        )
        .unwrap();

    let foreign_link = store.link_conversation(
        "intruder",
        "intruder-key",
        "fingerprint",
        7,
        None,
        None,
        &[],
        false,
    );
    assert!(matches!(foreign_link, Err(StoreError::ActorMismatch)));

    let foreign_runs = store.list_session_runs("intruder", &linked.session_id, 10);
    assert!(matches!(foreign_runs, Err(StoreError::ActorMismatch)));

    assert!(matches!(
        store.list_session_runs("local-user", "session-missing", 10),
        Err(StoreError::NotFound("session"))
    ));
}

#[test]
fn session_runs_list_newest_first_with_terminal_status() {
    let store = DurableStore::in_memory().unwrap();
    let linked = store
        .link_conversation(
            "local-user",
            "link-1",
            "fingerprint",
            9,
            None,
            None,
            &history(),
            false,
        )
        .unwrap();
    let second = store
        .start_run("local-user", "prompt-1", "prompt-1", &linked.session_id)
        .unwrap()
        .value;

    let runs = store
        .list_session_runs("local-user", &linked.session_id, 10)
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].run_id, second);
    assert_eq!(runs[0].status, RunStatus::Running);
    assert_eq!(runs[0].last_seq, 0);
    assert_eq!(runs[1].status, RunStatus::Completed);
    assert_eq!(runs[1].finish_reason.as_deref(), Some("import"));
    assert_eq!(runs[1].last_seq, 6);

    let limited = store
        .list_session_runs("local-user", &linked.session_id, 1)
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].run_id, second);

    let history_events = store
        .session_events(&linked.session_id, "local-user")
        .unwrap();
    assert!(matches!(
        history_events[0].event,
        CanonicalEvent::RunStarted(RunStarted { mode: Some(ref mode), .. }) if mode == "import"
    ));
    assert!(history_events
        .iter()
        .any(|event| matches!(&event.event, CanonicalEvent::RunCompleted(RunTerminal { reason, .. }) if reason == "import")));
}

#[test]
fn concurrent_links_create_exactly_one_session_and_import() {
    // Separate connections on one file exercise real SQLite contention, not
    // the in-process connection mutex.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    DurableStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for index in 0..4 {
        let path = path.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let store = DurableStore::open(&path).unwrap();
            barrier.wait();
            store
                .link_conversation(
                    "local-user",
                    &format!("link-{index}"),
                    "fingerprint",
                    77,
                    None,
                    None,
                    &history(),
                    false,
                )
                .unwrap()
        }));
    }
    let links = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    let session_id = links[0].session_id.clone();
    assert!(links.iter().all(|link| link.session_id == session_id));
    assert_eq!(
        links.iter().filter(|link| link.created).count(),
        1,
        "exactly one caller may create the import"
    );
    let store = DurableStore::open(&path).unwrap();
    let events = store.session_events(&session_id, "local-user").unwrap();
    assert_eq!(events.len(), 6, "one import run, no duplicated events");
}

#[test]
fn terminal_finish_reason_is_masked_and_listed() {
    let store = DurableStore::in_memory().unwrap();
    let session = store
        .create_session("local-user", "s", "s", None, None)
        .unwrap()
        .value;
    let run = store
        .start_run("local-user", "r", "r", &session)
        .unwrap()
        .value;
    let secret = "sk-abcdefghijklmnopqrstuvwx";
    store
        .append_event(
            "local-user",
            &EventEnvelope {
                event_id: "event-1".to_owned(),
                schema_version: V1Version::VALUE,
                session_id: session.clone(),
                run_id: run.clone(),
                item_id: None,
                seq: 1,
                occurred_at: "2026-09-01T00:00:00Z".to_owned(),
                actor: ActorRef {
                    id: "local-user".to_owned(),
                    kind: ActorKind::LocalUser,
                },
                source: "test".to_owned(),
                causation_id: None,
                correlation_id: None,
                event: CanonicalEvent::RunFailed(RunTerminal {
                    reason: secret.to_owned(),
                    error_code: Some("provider_error".to_owned()),
                }),
                extensions: Default::default(),
            },
        )
        .unwrap();
    let stored = store.all_events(&run, "local-user").unwrap();
    let CanonicalEvent::RunFailed(stored_terminal) = &stored.last().unwrap().event else {
        panic!("expected the terminal event");
    };
    assert_ne!(stored_terminal.reason, secret, "event log must mask");
    let runs = store.list_session_runs("local-user", &session, 10).unwrap();
    assert_eq!(
        runs[0].finish_reason.as_deref(),
        Some(stored_terminal.reason.as_str()),
        "run summary must match the masked event log"
    );
}

#[test]
fn version_one_store_upgrades_to_two_and_preserves_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let (session, run) = {
        let store = DurableStore::open(&path).unwrap();
        let session = store
            .create_session("local-user", "s", "s", Some("kept"), Some("project"))
            .unwrap()
            .value;
        let run = store
            .start_run("local-user", "r", "r", &session)
            .unwrap()
            .value;
        store
            .append_event(
                "local-user",
                &EventEnvelope {
                    event_id: "event-1".to_owned(),
                    schema_version: V1Version::VALUE,
                    session_id: session.clone(),
                    run_id: run.clone(),
                    item_id: None,
                    seq: 1,
                    occurred_at: "2026-09-01T00:00:00Z".to_owned(),
                    actor: ActorRef {
                        id: "local-user".to_owned(),
                        kind: ActorKind::LocalUser,
                    },
                    source: "test".to_owned(),
                    causation_id: None,
                    correlation_id: None,
                    event: CanonicalEvent::RunStarted(RunStarted {
                        model: None,
                        mode: Some("test".to_owned()),
                    }),
                    extensions: Default::default(),
                },
            )
            .unwrap();
        (session, run)
    };
    {
        // Simulate the schema-v1 layout: no link table, version 1.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "DROP TABLE rust_conversation_links; UPDATE rust_schema_meta SET version = 1;",
            )
            .unwrap();
    }
    let store = DurableStore::open(&path).unwrap();
    let snapshot = store.load_session(&session, "local-user").unwrap();
    assert_eq!(snapshot.title.as_deref(), Some("kept"));
    assert_eq!(snapshot.project_key.as_deref(), Some("project"));
    assert_eq!(store.all_events(&run, "local-user").unwrap().len(), 1);

    let linked = store
        .link_conversation(
            "local-user",
            "link-1",
            "fingerprint",
            3,
            Some("Created after upgrade"),
            None,
            &[],
            false,
        )
        .unwrap();
    assert!(linked.created, "v2 link table exists after the upgrade");
}
