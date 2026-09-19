//! Artifacts store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::artifacts::NewArtifact;
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

fn artifact(filename: &str, sha256: &str) -> NewArtifact {
    NewArtifact {
        filename: filename.to_string(),
        media_type: "text/plain".to_string(),
        kind: "file".to_string(),
        size_bytes: 3,
        sha256: Some(sha256.to_string()),
        storage_path: format!("{}/{}", &sha256[..2], sha256),
        ..NewArtifact::default()
    }
}

#[test]
fn artifact_crud_orders_newest_first_and_tracks_versions() {
    let (_directory, store) = adopted_store();

    let first = store
        .register_artifact(
            "local-user",
            1,
            &NewArtifact {
                metadata: Some(json!({"page_count": 1})),
                ..artifact("a.txt", "aaaa")
            },
        )
        .expect("register first");
    assert_eq!(first.version, 1);
    assert!(!first.is_deleted);
    assert_eq!(first.metadata, Some(json!({"page_count": 1})));

    let second = store
        .register_artifact("local-user", 1, &artifact("b.txt", "bbbb"))
        .expect("register second");
    let listed = store
        .list_artifacts("local-user", 1, false, None)
        .expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, second.id, "newest first");

    let versioned = store
        .register_artifact(
            "local-user",
            1,
            &NewArtifact {
                parent_id: Some(first.id),
                ..artifact("a-v2.txt", "cccc")
            },
        )
        .expect("versioned");
    assert_eq!(versioned.version, 2);
    assert_eq!(versioned.parent_id, Some(first.id));

    let found = store
        .find_artifact_by_sha256("local-user", 1, "aaaa")
        .expect("find")
        .expect("present");
    assert_eq!(found.id, first.id);

    store
        .set_artifact_extracted_text("local-user", first.id, "hello world")
        .expect("extract");
    let reloaded = store.get_artifact("local-user", first.id).expect("get");
    assert_eq!(reloaded.extracted_text.as_deref(), Some("hello world"));
    assert_eq!(
        store
            .get_artifact("local-user", second.id)
            .expect("second")
            .version,
        1
    );
}

#[test]
fn soft_deleted_artifacts_are_hidden_but_recoverable_with_include_deleted() {
    let (_directory, store) = adopted_store();
    let artifact = store
        .register_artifact("local-user", 1, &artifact("gone.txt", "dddd"))
        .expect("register");

    store
        .soft_delete_artifact("local-user", artifact.id)
        .expect("delete");
    assert!(
        store
            .list_artifacts("local-user", 1, false, None)
            .expect("live")
            .is_empty()
    );
    let all = store
        .list_artifacts("local-user", 1, true, None)
        .expect("all");
    assert_eq!(all.len(), 1);
    assert!(all[0].is_deleted);
    assert!(matches!(
        store.get_artifact("local-user", artifact.id),
        Err(StoreError::NotFound("artifact"))
    ));
    assert!(
        store
            .find_artifact_by_sha256("local-user", 1, "dddd")
            .expect("find")
            .is_none()
    );
    assert!(matches!(
        store.soft_delete_artifact("local-user", artifact.id),
        Err(StoreError::NotFound("artifact"))
    ));
}

#[test]
fn artifacts_are_scoped_to_the_conversation_owner() {
    let (_directory, store) = adopted_store();
    let mine = store
        .register_artifact("local-user", 1, &artifact("mine.txt", "eeee"))
        .expect("register");
    store.ensure_actor("other-actor").expect("actor");

    assert!(matches!(
        store.list_artifacts("other-actor", 1, false, None),
        Err(StoreError::NotFound("conversation"))
    ));
    assert!(matches!(
        store.get_artifact("other-actor", mine.id),
        Err(StoreError::NotFound("conversation"))
    ));
    assert!(matches!(
        store.register_artifact("other-actor", 1, &artifact("x.txt", "ffff")),
        Err(StoreError::NotFound("conversation"))
    ));
    assert!(matches!(
        store.set_artifact_extracted_text("other-actor", mine.id, "nope"),
        Err(StoreError::NotFound("conversation"))
    ));
    assert!(matches!(
        store.soft_delete_artifact("other-actor", mine.id),
        Err(StoreError::NotFound("conversation"))
    ));
    assert!(matches!(
        store.get_artifact("local-user", 9_999),
        Err(StoreError::NotFound("artifact"))
    ));
}
