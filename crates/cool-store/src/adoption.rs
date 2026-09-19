//! Adoption of an existing Python database at the cutover baseline.
//!
//! Adoption is the one-way point where Rust becomes the migration owner of the
//! user database:
//!
//! 1. the database must be at the supported Alembic revision;
//! 2. a verified backup is taken before the first Rust write;
//! 3. `rust_store_meta` records the owner and the adopted revision;
//! 4. Rust-owned migrations run incrementally and resumably.
//!
//! From then on a changed `alembic_version` (i.e. another tool ran migrations)
//! fails closed instead of silently mixing two migration owners.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::actors::{DEFAULT_ACTOR_ID, ensure_actor};
use crate::backup::{BackupReport, create_verified_backup, default_backup_path};
use crate::error::StoreError;
use crate::migrations::{meta_get, meta_set, run_pending, table_exists};
use crate::time::now_python;

/// The Alembic revision Rust adopts as its store baseline.
pub const SUPPORTED_ALEMBIC_REVISION: &str = "0022";

pub(crate) const META_OWNER: &str = "owner";
pub(crate) const META_ADOPTED_REVISION: &str = "adopted_alembic_revision";
pub(crate) const META_ADOPTED_AT: &str = "adopted_at";
pub(crate) const META_BACKUP_PATH: &str = "backup_path";
pub(crate) const META_INITIALIZED_FROM: &str = "initialized_from";

/// Outcome of adopting (or re-opening) an adopted database.
#[derive(Debug)]
pub struct AdoptionReport {
    /// True when this call performed the first adoption.
    pub adopted: bool,
    /// True when the database was created fresh from the baseline snapshot.
    pub initialized: bool,
    pub adopted_revision: String,
    pub backup: Option<BackupReport>,
    pub migrations_applied: Vec<i64>,
}

/// Read and validate the Alembic revision of a Python store.
pub(crate) fn read_revision(connection: &Connection) -> Result<String, StoreError> {
    if !table_exists(connection, "alembic_version")? {
        return Err(StoreError::NotALegacyStore);
    }
    let mut statement = connection.prepare("SELECT version_num FROM alembic_version")?;
    let revisions: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    match revisions.as_slice() {
        [revision] => Ok(revision.clone()),
        [] => Err(StoreError::Corruption(
            "alembic_version is empty".to_string(),
        )),
        _ => Err(StoreError::Corruption(format!(
            "alembic_version has {} rows",
            revisions.len()
        ))),
    }
}

/// Verify the revision is exactly the supported baseline.
pub(crate) fn verify_supported_revision(connection: &Connection) -> Result<String, StoreError> {
    let revision = read_revision(connection)?;
    if revision != SUPPORTED_ALEMBIC_REVISION {
        return Err(StoreError::UnsupportedLegacyRevision {
            found: revision,
            supported: SUPPORTED_ALEMBIC_REVISION,
        });
    }
    Ok(revision)
}

pub(crate) fn is_rust_owned(connection: &Connection) -> Result<bool, StoreError> {
    if !table_exists(connection, "rust_store_meta")? {
        return Ok(false);
    }
    Ok(meta_get(connection, META_OWNER)?.as_deref() == Some("rust"))
}

/// Adopt an existing Python database (or verify an already adopted one).
pub(crate) fn adopt(
    connection: &Connection,
    path: Option<&Path>,
    backup_dir: Option<&Path>,
) -> Result<AdoptionReport, StoreError> {
    let meta_exists = table_exists(connection, "rust_store_meta")?;
    if meta_exists {
        match meta_get(connection, META_OWNER)?.as_deref() {
            Some("rust") => {
                // Read the revision without the baseline check first: a changed
                // revision on a Rust-owned store means another migration tool
                // wrote to it, which is a different failure than an
                // unsupported legacy database.
                let current = read_revision(connection)?;
                let adopted = match meta_get(connection, META_ADOPTED_REVISION)? {
                    Some(adopted) => adopted,
                    None => {
                        // The process died between writing the owner marker and
                        // the adopted revision. That is resumable while the
                        // revision is still the supported baseline; anything
                        // else means the file was migrated underneath us.
                        if current != SUPPORTED_ALEMBIC_REVISION {
                            return Err(StoreError::MigrationOwnershipConflict(format!(
                                "database has a partial Rust ownership marker and is at \
                                 Alembic revision {current}, not the supported baseline"
                            )));
                        }
                        let transaction = crate::migrations::immediate_transaction(connection)?;
                        meta_set(&transaction, META_ADOPTED_REVISION, &current)?;
                        meta_set(&transaction, META_ADOPTED_AT, &now_python())?;
                        transaction.commit()?;
                        current.clone()
                    }
                };
                if adopted != current {
                    return Err(StoreError::MigrationOwnershipConflict(format!(
                        "database was adopted at Alembic revision {adopted} but is now at \
                         {current}; another migration tool wrote to a Rust-owned store"
                    )));
                }
                let migrations_applied = run_pending(connection)?;
                ensure_actor(connection, DEFAULT_ACTOR_ID)?;
                return Ok(AdoptionReport {
                    adopted: false,
                    initialized: false,
                    adopted_revision: adopted,
                    backup: None,
                    migrations_applied,
                });
            }
            Some(other) => {
                return Err(StoreError::MigrationOwnershipConflict(format!(
                    "database is owned by {other:?}, not by the Rust store"
                )));
            }
            None => {
                return Err(StoreError::Corruption(
                    "rust_store_meta exists without an owner marker".to_string(),
                ));
            }
        }
    }

    let revision = verify_supported_revision(connection)?;
    let backup = match path {
        Some(database) => {
            let target = default_backup_path(database, backup_dir);
            Some(create_verified_backup(connection, &target)?)
        }
        None => None,
    };
    let timestamp = now_python();
    // The meta table, the ownership marker and its adopted revision commit in
    // a single transaction, so a crash can never leave a Rust-opaque database
    // (an empty meta table or an owner without a revision).
    let transaction = crate::migrations::immediate_transaction(connection)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS rust_store_meta(
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );",
    )?;
    meta_set(&transaction, META_OWNER, "rust")?;
    meta_set(&transaction, META_ADOPTED_REVISION, &revision)?;
    meta_set(&transaction, META_ADOPTED_AT, &timestamp)?;
    if let Some(report) = &backup {
        meta_set(
            &transaction,
            META_BACKUP_PATH,
            &report.path.to_string_lossy(),
        )?;
    }
    transaction.commit()?;
    let migrations_applied = run_pending(connection)?;
    ensure_actor(connection, DEFAULT_ACTOR_ID)?;
    Ok(AdoptionReport {
        adopted: true,
        initialized: false,
        adopted_revision: revision,
        backup,
        migrations_applied,
    })
}

/// Create a fresh database from the committed baseline snapshot and mark it
/// Rust-owned. Used for new installations and in-memory test stores.
pub(crate) fn initialize(connection: &Connection) -> Result<AdoptionReport, StoreError> {
    // The baseline schema and the ownership markers commit in one transaction,
    // so a fresh database can never be left half-created.
    let transaction = crate::migrations::immediate_transaction(connection)?;
    transaction.execute_batch(crate::BASELINE_SCHEMA_SQL)?;
    let timestamp = now_python();
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS rust_store_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    meta_set(&transaction, META_OWNER, "rust")?;
    meta_set(
        &transaction,
        META_ADOPTED_REVISION,
        SUPPORTED_ALEMBIC_REVISION,
    )?;
    meta_set(&transaction, META_ADOPTED_AT, &timestamp)?;
    meta_set(&transaction, META_INITIALIZED_FROM, "baseline-0022")?;
    transaction.commit()?;
    let migrations_applied = run_pending(connection)?;
    ensure_actor(connection, DEFAULT_ACTOR_ID)?;
    Ok(AdoptionReport {
        adopted: false,
        initialized: true,
        adopted_revision: SUPPORTED_ALEMBIC_REVISION.to_string(),
        backup: None,
        migrations_applied,
    })
}

/// Recorded ownership/revision metadata of a store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreMeta {
    pub owner: Option<String>,
    pub adopted_alembic_revision: Option<String>,
    pub adopted_at: Option<String>,
    pub backup_path: Option<String>,
    pub initialized_from: Option<String>,
    pub schema_version: i64,
}

pub(crate) fn read_meta(connection: &Connection) -> Result<StoreMeta, StoreError> {
    if !table_exists(connection, "rust_store_meta")? {
        return Ok(StoreMeta {
            owner: None,
            adopted_alembic_revision: None,
            adopted_at: None,
            backup_path: None,
            initialized_from: None,
            schema_version: 0,
        });
    }
    Ok(StoreMeta {
        owner: meta_get(connection, META_OWNER)?,
        adopted_alembic_revision: meta_get(connection, META_ADOPTED_REVISION)?,
        adopted_at: meta_get(connection, META_ADOPTED_AT)?,
        backup_path: meta_get(connection, META_BACKUP_PATH)?,
        initialized_from: meta_get(connection, META_INITIALIZED_FROM)?,
        schema_version: crate::migrations::schema_version(connection)?,
    })
}

/// Read the current Alembic revision of a Rust-owned store (diagnostics/CLI).
pub fn store_alembic_revision(connection: &Connection) -> Result<Option<String>, StoreError> {
    if !table_exists(connection, "alembic_version")? {
        return Ok(None);
    }
    Ok(connection
        .query_row("SELECT version_num FROM alembic_version", [], |row| {
            row.get(0)
        })
        .optional()?)
}
