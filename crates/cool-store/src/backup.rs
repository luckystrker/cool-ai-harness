//! Verified backup and restore for the adopted database.
//!
//! The first Rust write into a user database requires a checked backup copy.
//! `VACUUM INTO` produces a consistent snapshot even when the source is in WAL
//! mode; the copy is then opened read-only and verified (integrity, Alembic
//! revision and row counts) before it is trusted.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::migrations::table_exists;
use crate::time::now_python;

/// Tables whose row counts are compared between source and backup.
const VERIFIED_TABLES: &[&str] = &[
    "users",
    "conversations",
    "messages",
    "agent_runs",
    "run_events",
    "tool_calls",
    "artifacts",
    "memory_items",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub revision: String,
    pub integrity_check: String,
    pub verified_tables: Vec<(String, i64)>,
}

/// Create and verify a backup of `connection` at `target`.
///
/// The target must not exist yet; callers pass a unique, timestamped name.
pub fn create_verified_backup(
    connection: &Connection,
    target: &Path,
) -> Result<BackupReport, StoreError> {
    if target.exists() {
        return Err(StoreError::BackupFailed(format!(
            "backup target {} already exists",
            target.display()
        )));
    }
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let escaped = target.to_string_lossy().replace('\'', "''");
    connection
        .execute_batch(&format!("VACUUM INTO '{escaped}';"))
        .map_err(|error| StoreError::BackupFailed(format!("VACUUM INTO failed: {error}")))?;
    verify_backup(connection, target)
}

/// Open a backup read-only and measure it (integrity, revision, row counts).
///
/// Unlike [`verify_backup`] this compares against nothing: it is the
/// standalone check used before restoring a backup whose source database may
/// no longer exist or may already have diverged.
pub fn inspect_backup(backup: &Path) -> Result<BackupReport, StoreError> {
    let connection = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| {
            StoreError::BackupFailed(format!("cannot open backup {}: {error}", backup.display()))
        })?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let integrity_check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| StoreError::BackupFailed(format!("integrity_check failed: {error}")))?;
    if integrity_check != "ok" {
        return Err(StoreError::BackupFailed(format!(
            "integrity_check returned {integrity_check:?}"
        )));
    }
    let revision = crate::adoption::read_revision(&connection)?;
    let mut verified_tables = Vec::new();
    for table in VERIFIED_TABLES {
        if !table_exists(&connection, table)? {
            continue;
        }
        let count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        verified_tables.push((table.to_string(), count));
    }
    drop(connection);
    let bytes = std::fs::metadata(backup)?.len();
    let sha256 = hash_file(backup)?;
    Ok(BackupReport {
        path: backup.to_path_buf(),
        bytes,
        sha256,
        revision,
        integrity_check,
        verified_tables,
    })
}

/// Verify a backup against a live source connection (used while creating one).
pub fn verify_backup(source: &Connection, backup: &Path) -> Result<BackupReport, StoreError> {
    let report = inspect_backup(backup)?;
    let expected = crate::adoption::read_revision(source)?;
    if report.revision != expected {
        return Err(StoreError::BackupFailed(format!(
            "backup is at revision {}, source is at {expected}",
            report.revision
        )));
    }
    for (table, backup_count) in &report.verified_tables {
        let source_count: i64 =
            source.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        if *backup_count != source_count {
            return Err(StoreError::BackupFailed(format!(
                "table {table} has {backup_count} rows in the backup and {source_count} in the source"
            )));
        }
    }
    Ok(report)
}

/// Restore a verified backup over `target`.
///
/// The backup is inspected standalone: it must have a clean integrity check
/// and be at the supported Alembic baseline. Any live store must be dropped
/// first. Stale `-wal`/`-shm` sidecars are removed before and after the file
/// replacement so a previous WAL cannot be replayed over the restored file.
pub fn restore_backup(backup: &Path, target: &Path) -> Result<BackupReport, StoreError> {
    let same_file = match (backup.canonicalize(), target.canonicalize()) {
        (Ok(backup), Ok(target)) => backup == target,
        _ => backup == target,
    };
    if same_file {
        return Err(StoreError::InvalidInput(
            "backup and restore target are the same file".to_string(),
        ));
    }
    let report = inspect_backup(backup)?;
    if report.revision != crate::adoption::SUPPORTED_ALEMBIC_REVISION {
        return Err(StoreError::UnsupportedLegacyRevision {
            found: report.revision,
            supported: crate::adoption::SUPPORTED_ALEMBIC_REVISION,
        });
    }
    remove_sidecar(target, "-wal")?;
    remove_sidecar(target, "-shm")?;
    if target.exists() {
        let staging = target.with_extension("restore-staging");
        std::fs::copy(backup, &staging)?;
        std::fs::rename(&staging, target)?;
    } else {
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(backup, target)?;
    }
    remove_sidecar(target, "-wal")?;
    remove_sidecar(target, "-shm")?;
    Ok(report)
}

fn remove_sidecar(target: &Path, suffix: &str) -> Result<(), StoreError> {
    let sidecar = PathBuf::from(format!("{}{}", target.display(), suffix));
    if sidecar.exists() {
        std::fs::remove_file(sidecar)?;
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, StoreError> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Build a unique, timestamped backup path next to the database.
pub(crate) fn default_backup_path(database: &Path, backup_dir: Option<&Path>) -> PathBuf {
    let file_name = database
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "harness".to_string());
    let timestamp = now_python().replace([' ', ':', '-'], "");
    let directory = backup_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        database
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let mut candidate = directory.join(format!("{file_name}.backup-{timestamp}.db"));
    let mut counter = 1;
    while candidate.exists() {
        candidate = directory.join(format!("{file_name}.backup-{timestamp}-{counter}.db"));
        counter += 1;
    }
    candidate
}

/// Read the Alembic revision of an arbitrary database path (used by tests and
/// restore tooling).
pub fn peek_alembic_revision(path: &Path) -> Result<Option<String>, StoreError> {
    if !path.exists() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let revision: Option<String> = connection
        .query_row("SELECT version_num FROM alembic_version", [], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(revision)
}
