//! Rust-owned store for the existing Python (SQLModel/Alembic) SQLite schema.
//!
//! M6-M9 kept the Rust runtime in a separate, `rust_`-namespaced database. M10
//! adds the store-cutover layer: this crate opens the real `harness.db`, proves
//! the schema is at the supported Alembic baseline, takes a verified backup
//! before the first Rust write, records migration ownership in
//! `rust_store_meta`, and exposes typed access to every legacy subsystem the
//! React surface and the background workers use.
//!
//! Design rules:
//!
//! - The Alembic baseline schema is a committed snapshot
//!   (`tests/fixtures/python_schema_0022.sql`) generated from the real
//!   migration chain by `backend/tests/schema_snapshot.py`.
//! - Rust never runs Alembic and fails closed if `alembic_version` changes
//!   after adoption, so two migration owners can never interleave.
//! - Connection pragmas mirror the Python engine (`WAL`, `synchronous=NORMAL`,
//!   `busy_timeout=5000`, foreign keys left at SQLite's default of off) so both
//!   runtimes treat constraint failures identically.
//! - `read_only` mode opens a Python database without taking ownership or
//!   writing anything, for parity/replay work.

mod actors;
mod adoption;
mod backup;
pub mod domains;
mod error;
mod idempotency;
pub mod memory;
mod migrations;
pub mod observability;
pub mod scheduler;
pub mod time;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OpenFlags};

pub use actors::{DEFAULT_ACTOR_ID, actor_for_user};
pub use adoption::{AdoptionReport, SUPPORTED_ALEMBIC_REVISION, StoreMeta, store_alembic_revision};
pub use backup::{
    BackupReport, create_verified_backup, inspect_backup, peek_alembic_revision, restore_backup,
    verify_backup,
};
pub use error::StoreError;
pub use idempotency::Idempotent;
pub use time::{iso_weekday, now_python, parse_python_datetime, python_datetime};

/// The latest Rust-owned schema version.
pub fn latest_schema_version() -> i64 {
    migrations::MIGRATIONS
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0)
}

/// Committed baseline schema snapshot (Alembic revision 0022).
pub const BASELINE_SCHEMA_SQL: &str = include_str!("../tests/fixtures/python_schema_0022.sql");
/// SHA-256 of the committed baseline snapshot.
pub const BASELINE_SCHEMA_SHA256: &str =
    include_str!("../tests/fixtures/python_schema_0022.sql.sha256");

/// Options controlling how a legacy store is opened.
#[derive(Clone, Debug, Default)]
pub struct StoreOptions {
    /// Directory for pre-adoption backups (defaults to the database directory).
    pub backup_dir: Option<PathBuf>,
    /// Create a fresh database from the baseline snapshot when the file is absent.
    pub initialize_if_missing: bool,
    /// Open without adopting, migrating or writing.
    pub read_only: bool,
}

/// A handle to the Python-schema database.
#[derive(Clone, Debug)]
pub struct LegacyStore {
    connection: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
    adoption: Option<Arc<AdoptionReport>>,
}

impl LegacyStore {
    /// Open (and adopt, when needed) an existing Python database.
    pub fn open(path: impl AsRef<Path>, options: &StoreOptions) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if options.read_only {
            return Self::open_read_only(&path);
        }
        if !path.exists() {
            if !options.initialize_if_missing {
                return Err(StoreError::NotFound("database"));
            }
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            let connection = Connection::open(&path)?;
            configure_connection(&connection)?;
            configure_pragmas(&connection)?;
            let adoption = adoption::initialize(&connection)?;
            return Ok(Self {
                connection: Arc::new(Mutex::new(connection)),
                path: Some(path),
                adoption: Some(Arc::new(adoption)),
            });
        }
        let connection = Connection::open(&path)?;
        configure_connection(&connection)?;
        let adoption = adoption::adopt(&connection, Some(&path), options.backup_dir.as_deref())?;
        // WAL/synchronous/foreign-keys are set only after the pre-adoption
        // backup exists, so the backup literally precedes the first write.
        configure_pragmas(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: Some(path),
            adoption: Some(Arc::new(adoption)),
        })
    }

    /// Open an existing database for reading only; no adoption or migration.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(StoreError::NotFound("database"));
        }
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: Some(path),
            adoption: None,
        })
    }

    /// Create a fresh database from the committed baseline snapshot.
    pub fn initialize(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(&path)?;
        configure_connection(&connection)?;
        configure_pragmas(&connection)?;
        let adoption = adoption::initialize(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: Some(path),
            adoption: Some(Arc::new(adoption)),
        })
    }

    /// In-memory store created from the baseline snapshot (tests only).
    pub fn in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        configure_connection(&connection)?;
        configure_pragmas(&connection)?;
        let adoption = adoption::initialize(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path: None,
            adoption: Some(Arc::new(adoption)),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn adoption_report(&self) -> Option<&AdoptionReport> {
        self.adoption.as_deref()
    }

    pub fn meta(&self) -> Result<StoreMeta, StoreError> {
        let connection = self.connection()?;
        adoption::read_meta(&connection)
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        let connection = self.connection()?;
        migrations::schema_version(&connection)
    }

    pub fn is_rust_owned(&self) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        adoption::is_rust_owned(&connection)
    }

    pub fn alembic_revision(&self) -> Result<Option<String>, StoreError> {
        let connection = self.connection()?;
        store_alembic_revision(&connection)
    }

    /// Create a verified backup at `target`.
    pub fn create_backup(&self, target: impl AsRef<Path>) -> Result<BackupReport, StoreError> {
        let connection = self.connection()?;
        create_verified_backup(&connection, target.as_ref())
    }

    /// Register an actor in the legacy user mapping (idempotent).
    pub fn ensure_actor(&self, actor_id: &str) -> Result<i64, StoreError> {
        let connection = self.connection()?;
        actors::ensure_actor(&connection, actor_id)
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Corruption("connection mutex poisoned".to_owned()))
    }

    /// Raw connection access for store migrations, diagnostics and tests.
    ///
    /// This bypasses actor scoping and must not be used by application or
    /// protocol code; those go through the typed domain methods.
    #[doc(hidden)]
    pub fn with_connection<T>(
        &self,
        action: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let connection = self.connection()?;
        action(&connection)
    }
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

fn configure_pragmas(connection: &Connection) -> Result<(), StoreError> {
    // Mirror the Python engine exactly: WAL + synchronous=NORMAL + busy_timeout,
    // and foreign keys explicitly off. The Python runtime never enables
    // `PRAGMA foreign_keys`, so Rust must disable it too (rusqlite's bundled
    // SQLite compiles with FK enforcement on by default) or the same delete
    // would fail on one runtime and leave orphans on the other.
    connection.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = OFF;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn committed_snapshot_digest_matches_the_manifest() {
        let mut hasher = Sha256::new();
        hasher.update(BASELINE_SCHEMA_SQL.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        assert_eq!(digest, BASELINE_SCHEMA_SHA256.trim());
    }

    #[test]
    fn in_memory_store_is_owned_and_migrated() {
        let store = LegacyStore::in_memory().expect("store");
        let meta = store.meta().expect("meta");
        assert_eq!(meta.owner.as_deref(), Some("rust"));
        assert_eq!(
            meta.adopted_alembic_revision.as_deref(),
            Some(SUPPORTED_ALEMBIC_REVISION)
        );
        assert!(meta.schema_version >= 1);
        assert!(store.is_rust_owned().expect("owned"));
    }
}
