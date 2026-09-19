//! Error type for the legacy Python-schema store.

use std::fmt;

/// Errors returned by `cool-store`.
#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    NotFound(&'static str),
    InvalidInput(String),
    Conflict(String),
    Corruption(String),
    /// The database is not a migrated Python store (no `alembic_version` row).
    NotALegacyStore,
    /// The Alembic revision is not the supported cutover baseline.
    UnsupportedLegacyRevision {
        found: String,
        supported: &'static str,
    },
    /// Another migration owner changed the schema after Rust adopted it.
    MigrationOwnershipConflict(String),
    /// The actor is not registered in `rust_actors`.
    ActorUnknown(String),
    BackupFailed(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "SQLite error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::NotFound(kind) => write!(formatter, "{kind} not found"),
            Self::InvalidInput(message) => write!(formatter, "invalid input: {message}"),
            Self::Conflict(message) => write!(formatter, "conflict: {message}"),
            Self::Corruption(message) => write!(formatter, "store is corrupt: {message}"),
            Self::NotALegacyStore => formatter.write_str(
                "database has no Alembic history; run the Python migration chain before adoption",
            ),
            Self::UnsupportedLegacyRevision { found, supported } => write!(
                formatter,
                "legacy schema revision {found} is not the supported cutover baseline {supported}"
            ),
            Self::MigrationOwnershipConflict(message) => {
                write!(formatter, "migration ownership conflict: {message}")
            }
            Self::ActorUnknown(actor) => write!(formatter, "unknown actor {actor}"),
            Self::BackupFailed(message) => write!(formatter, "backup failed: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
