//! Adoption, backup/restore and migration-ownership tests (M10 exit criteria).

mod common;

use std::path::PathBuf;

use common::{DEFAULT_SEED, create_python_database, has_table, table_count, temp_database};
use cool_store::{LegacyStore, StoreError, StoreOptions, restore_backup, verify_backup};
use rusqlite::Connection;
use tempfile::TempDir;

fn options() -> StoreOptions {
    StoreOptions {
        initialize_if_missing: false,
        ..StoreOptions::default()
    }
}

#[test]
fn adopting_an_existing_database_backs_it_up_and_preserves_data() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);

    let store = LegacyStore::open(&path, &options()).expect("adopt");
    let report = store.adoption_report().expect("adoption report");
    assert!(report.adopted);
    assert_eq!(report.adopted_revision, "0022");
    assert_eq!(
        report.migrations_applied,
        (1..=cool_store::latest_schema_version()).collect::<Vec<_>>()
    );
    let backup = report.backup.as_ref().expect("backup");
    assert_eq!(backup.revision, "0022");
    assert_eq!(backup.integrity_check, "ok");
    assert!(
        backup
            .verified_tables
            .contains(&("conversations".to_string(), 1))
    );
    assert!(backup.path.exists());
    let backup_store = LegacyStore::open_read_only(&backup.path).expect("open backup");
    assert_eq!(
        backup_store
            .list_conversations("local-user", &Default::default())
            .expect("list")
            .len(),
        1
    );

    let meta = store.meta().expect("meta");
    assert_eq!(meta.owner.as_deref(), Some("rust"));
    assert_eq!(meta.adopted_alembic_revision.as_deref(), Some("0022"));
    assert_eq!(
        meta.backup_path.as_deref(),
        Some(backup.path.to_string_lossy().as_ref())
    );
    assert_eq!(
        store.alembic_revision().expect("revision").as_deref(),
        Some("0022")
    );

    // Legacy rows are untouched and visible through the typed API.
    let conversations = store
        .list_conversations("local-user", &Default::default())
        .expect("list conversations");
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].title.as_deref(), Some("Legacy chat"));
    assert_eq!(
        store
            .list_messages("local-user", 1, &Default::default())
            .expect("messages")
            .len(),
        1
    );
}

#[test]
fn reopening_an_adopted_database_is_idempotent_and_takes_no_new_backup() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);

    let first = LegacyStore::open(&path, &options()).expect("first adopt");
    let backup_path = first
        .adoption_report()
        .and_then(|report| report.backup.clone())
        .expect("backup")
        .path;
    drop(first);

    let second = LegacyStore::open(&path, &options()).expect("reopen");
    let report = second.adoption_report().expect("report");
    assert!(!report.adopted);
    assert!(report.backup.is_none());
    assert!(report.migrations_applied.is_empty());
    assert_eq!(
        second.schema_version().expect("version"),
        cool_store::latest_schema_version()
    );
    assert!(backup_path.exists());
}

#[test]
fn a_changed_alembic_revision_fails_closed_after_adoption() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let store = LegacyStore::open(&path, &options()).expect("adopt");
    drop(store);

    let connection = Connection::open(&path).expect("open");
    connection
        .execute("UPDATE alembic_version SET version_num = '0023'", [])
        .expect("simulate another migration owner");
    drop(connection);

    let error = LegacyStore::open(&path, &options()).expect_err("must fail closed");
    let message = error.to_string();
    assert!(
        matches!(error, StoreError::MigrationOwnershipConflict(detail) if detail.contains("0023")),
        "unexpected error: {message}"
    );
}

#[test]
fn an_unsupported_revision_fails_closed_before_any_write() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let connection = Connection::open(&path).expect("open");
    connection
        .execute("UPDATE alembic_version SET version_num = '0021'", [])
        .expect("set revision");
    drop(connection);

    let error = LegacyStore::open(&path, &options()).expect_err("must fail closed");
    assert!(matches!(
        error,
        StoreError::UnsupportedLegacyRevision { found, supported }
            if found == "0021" && supported == "0022"
    ));
    assert!(!has_table(&path, "rust_store_meta"));
}

#[test]
fn a_database_without_alembic_history_is_rejected() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "plain.db");
    Connection::open(&path)
        .expect("open")
        .execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY);")
        .expect("schema");

    let error = LegacyStore::open(&path, &options()).expect_err("must reject");
    assert!(matches!(error, StoreError::NotALegacyStore));
}

#[test]
fn an_interrupted_adoption_resumes_pending_migrations() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    // Simulate a crash after the ownership marker was written but before the
    // adopted revision and the Rust migrations were recorded (the hardest
    // interruption point: owner present, revision missing).
    let connection = Connection::open(&path).expect("open");
    connection
        .execute_batch(
            "CREATE TABLE rust_store_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO rust_store_meta(key, value) VALUES ('owner', 'rust');",
        )
        .expect("marker");
    drop(connection);

    let store = LegacyStore::open(&path, &options()).expect("resume adoption");
    let report = store.adoption_report().expect("report");
    assert!(!report.adopted);
    assert_eq!(report.adopted_revision, "0022");
    assert_eq!(
        report.migrations_applied,
        (1..=cool_store::latest_schema_version()).collect::<Vec<_>>()
    );
    assert!(has_table(&path, "rust_actors"));
    // The default actor mapping is restored on the resume path as well.
    assert_eq!(store.ensure_actor("local-user").expect("actor"), 1);
    store
        .list_conversations("local-user", &Default::default())
        .expect("actor scoping works after resume");
}

#[test]
fn a_partial_ownership_marker_at_another_revision_fails_closed() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let connection = Connection::open(&path).expect("open");
    connection
        .execute_batch(
            "CREATE TABLE rust_store_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO rust_store_meta(key, value) VALUES ('owner', 'rust');
             UPDATE alembic_version SET version_num = '0023';",
        )
        .expect("marker");
    drop(connection);

    let error = LegacyStore::open(&path, &options()).expect_err("must fail closed");
    assert!(matches!(error, StoreError::MigrationOwnershipConflict(_)));
}

#[test]
fn backup_and_restore_round_trips_the_pre_backup_state() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let backup_path = directory.path().join("manual-backup.db");

    let store = LegacyStore::open(&path, &options()).expect("adopt");
    let backup = store.create_backup(&backup_path).expect("backup");
    assert_eq!(backup.revision, "0022");
    let source = Connection::open(&path).expect("source");
    let verified = verify_backup(&source, &backup_path).expect("verify");
    assert_eq!(verified.sha256, backup.sha256);
    drop(source);
    store
        .create_conversation("local-user", &Default::default())
        .expect("write after backup");
    assert_eq!(table_count(&path, "conversations"), 2);
    assert!(has_table(&path, "rust_store_meta"));
    drop(store);

    restore_backup(&backup_path, &path).expect("restore");
    assert_eq!(table_count(&path, "conversations"), 1);
    assert!(has_table(&path, "rust_store_meta"));

    // The backup captured the already-adopted store, so the restored database
    // reopens without a new backup and still lists the original conversation.
    let reopened = LegacyStore::open(&path, &options()).expect("reopen");
    assert!(!reopened.adoption_report().expect("report").adopted);
    assert_eq!(
        reopened
            .list_conversations("local-user", &Default::default())
            .expect("list")
            .len(),
        1
    );
}

#[test]
fn restoring_the_adoption_backup_rolls_back_rust_ownership() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);

    let store = LegacyStore::open(&path, &options()).expect("adopt");
    let adoption_backup = store
        .adoption_report()
        .and_then(|report| report.backup.clone())
        .expect("adoption backup")
        .path;
    store
        .create_conversation("local-user", &Default::default())
        .expect("Rust-written row");
    assert_eq!(table_count(&path, "conversations"), 2);
    assert!(has_table(&path, "rust_store_meta"));
    drop(store);

    let report = restore_backup(&adoption_backup, &path).expect("restore");
    assert_eq!(report.revision, "0022");
    assert_eq!(table_count(&path, "conversations"), 1);
    assert!(!has_table(&path, "rust_store_meta"));
    assert_eq!(
        cool_store::peek_alembic_revision(&path)
            .expect("revision")
            .as_deref(),
        Some("0022")
    );
}

#[test]
fn restoring_a_backup_at_an_unsupported_revision_fails_closed() {
    let directory = TempDir::new().expect("tempdir");
    let source = temp_database(directory.path(), "harness.db");
    create_python_database(&source, DEFAULT_SEED);
    let connection = Connection::open(&source).expect("open");
    connection
        .execute("UPDATE alembic_version SET version_num = '0021'", [])
        .expect("downgrade fixture");
    drop(connection);
    let backup = directory.path().join("old-backup.db");
    std::fs::copy(&source, &backup).expect("copy");

    let target = temp_database(directory.path(), "target.db");
    create_python_database(&target, DEFAULT_SEED);
    let error = restore_backup(&backup, &target).expect_err("must fail closed");
    assert!(matches!(
        error,
        StoreError::UnsupportedLegacyRevision { found, supported }
            if found == "0021" && supported == "0022"
    ));
    // The target was not touched.
    assert_eq!(
        cool_store::peek_alembic_revision(&target)
            .expect("revision")
            .as_deref(),
        Some("0022")
    );
}

#[test]
fn restoring_a_backup_over_itself_is_rejected() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let error = restore_backup(&path, &path).expect_err("must reject");
    assert!(matches!(error, StoreError::InvalidInput(_)));

    // A non-normalized alias of the same file is rejected too. `Path` equality
    // skips `.` components on its own, so use a `..` path that only
    // canonicalization resolves.
    let nested = directory.path().join("nested");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let alias = nested.join("..").join("harness.db");
    assert_ne!(alias, path);
    let error = restore_backup(&path, &alias).expect_err("alias must reject");
    assert!(matches!(error, StoreError::InvalidInput(_)));
}

#[test]
fn read_only_open_never_adopts_or_writes() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);

    let store = LegacyStore::open_read_only(&path).expect("read only");
    assert!(!store.is_rust_owned().expect("owned"));
    assert_eq!(
        store
            .list_conversations("local-user", &Default::default())
            .expect("list")
            .len(),
        1
    );
    let error = store
        .create_conversation("local-user", &Default::default())
        .expect_err("writes must fail in read-only mode");
    assert!(matches!(error, StoreError::Sqlite(_)));
    drop(store);
    assert!(!has_table(&path, "rust_store_meta"));
}

#[test]
fn initialize_creates_a_rust_owned_store_without_a_backup() {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "fresh.db");
    let store = LegacyStore::open(
        &path,
        &StoreOptions {
            initialize_if_missing: true,
            ..StoreOptions::default()
        },
    )
    .expect("initialize");
    let report = store.adoption_report().expect("report");
    assert!(report.initialized);
    assert!(report.backup.is_none());
    assert_eq!(
        store.schema_version().expect("version"),
        cool_store::latest_schema_version()
    );
    assert!(store.meta().expect("meta").backup_path.is_none());
    assert_eq!(
        store
            .list_conversations("local-user", &Default::default())
            .expect("list")
            .len(),
        0
    );
}

#[test]
fn missing_database_without_initialize_flag_is_not_found() {
    let directory = TempDir::new().expect("tempdir");
    let path: PathBuf = temp_database(directory.path(), "absent.db");
    let error = LegacyStore::open(&path, &options()).expect_err("must not create");
    assert!(matches!(error, StoreError::NotFound("database")));
}
