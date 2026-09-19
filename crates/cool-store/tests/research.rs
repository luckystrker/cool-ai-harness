//! Research runs store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::research::NewResearchRun;
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

fn new_run(topic: &str) -> NewResearchRun {
    NewResearchRun {
        topic: topic.to_string(),
        model: Some("gpt-4o".to_string()),
        ..NewResearchRun::default()
    }
}

#[test]
fn research_lifecycle_create_finish_and_cancel() {
    let (_directory, store) = adopted_store();
    let run = store
        .create_research_run(
            "local-user",
            &NewResearchRun {
                conversation_id: Some(1),
                ..new_run("Quantum computing")
            },
        )
        .expect("create");
    assert_eq!(run.status, "running");
    assert_eq!(run.depth, 4);
    assert_eq!(run.conversation_id, Some(1));
    assert_eq!(run.input_hash.as_deref().map(str::len), Some(64));

    let finished = store
        .finish_research_run(
            "local-user",
            run.id,
            "completed",
            Some("# Report"),
            Some(&json!([{"url": "https://example.com"}])),
            Some(&json!([{"index": 1}])),
            Some(&json!(["sub question?"])),
            Some(&json!({"total_tokens": 42})),
            None,
            None,
        )
        .expect("finish");
    assert_eq!(finished.status, "completed");
    assert!(finished.finished_at.is_some());
    assert_eq!(finished.report_markdown.as_deref(), Some("# Report"));
    assert_eq!(
        finished.sources,
        Some(json!([{"url": "https://example.com"}]))
    );
    assert_eq!(finished.usage, Some(json!({"total_tokens": 42})));

    let second = store
        .create_research_run("local-user", &new_run("Second"))
        .expect("second");
    assert_eq!(
        store
            .list_research_runs("local-user", None)
            .expect("list")
            .first()
            .expect("first")
            .id,
        second.id,
        "newest first"
    );

    let cancelled = store
        .cancel_research_run("local-user", second.id)
        .expect("cancel");
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.finished_at.is_some());
    assert!(matches!(
        store.cancel_research_run("local-user", second.id),
        Err(StoreError::NotFound("research run"))
    ));
    assert!(matches!(
        store.finish_research_run(
            "local-user",
            run.id,
            "running",
            None,
            None,
            None,
            None,
            None,
            None,
            None
        ),
        Err(StoreError::InvalidInput(_))
    ));
}

#[test]
fn research_validation_rejects_bad_depth_and_conversation() {
    let (_directory, store) = adopted_store();
    assert!(matches!(
        store.create_research_run(
            "local-user",
            &NewResearchRun {
                depth: 9,
                ..new_run("Too deep")
            }
        ),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(matches!(
        store.create_research_run(
            "local-user",
            &NewResearchRun {
                conversation_id: Some(9_999),
                ..new_run("No conversation")
            }
        ),
        Err(StoreError::NotFound("conversation"))
    ));
}

#[test]
fn research_runs_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let run = store
        .create_research_run("local-user", &new_run("Mine"))
        .expect("create");
    store.ensure_actor("other-actor").expect("actor");

    assert!(
        store
            .list_research_runs("other-actor", None)
            .expect("list")
            .is_empty()
    );
    assert!(matches!(
        store.get_research_run("other-actor", run.id),
        Err(StoreError::NotFound("research run"))
    ));
    assert!(matches!(
        store.cancel_research_run("other-actor", run.id),
        Err(StoreError::NotFound("research run"))
    ));
    assert!(matches!(
        store.finish_research_run(
            "other-actor",
            run.id,
            "completed",
            None,
            None,
            None,
            None,
            None,
            None,
            None
        ),
        Err(StoreError::NotFound("research run"))
    ));
}
