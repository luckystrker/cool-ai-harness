//! Incremental Rust-owned migrations for an adopted Python database.
//!
//! After adoption the Rust store is the only migration owner. Every step runs
//! in its own `BEGIN IMMEDIATE` transaction and records its version in
//! `rust_store_meta`, so an interrupted run resumes from the last committed
//! step instead of leaving a half-applied schema.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::error::StoreError;

/// Begin an `IMMEDIATE` transaction so write intent and lock acquisition are
/// explicit and documented behaviour (rusqlite's `unchecked_transaction` is
/// `BEGIN DEFERRED`).
pub(crate) fn immediate_transaction(
    connection: &Connection,
) -> Result<Transaction<'_>, StoreError> {
    Ok(Transaction::new_unchecked(
        connection,
        TransactionBehavior::Immediate,
    )?)
}

/// A single schema migration for the Rust-owned portion of the store.
#[derive(Clone, Copy)]
pub(crate) struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Ordered migrations. Version numbers must be contiguous from 1.
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "rust_actors",
        sql: "CREATE TABLE rust_actors(
                actor_id TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL UNIQUE REFERENCES users(id),
                display_name TEXT,
                created_at TEXT NOT NULL
              );
              CREATE INDEX ix_rust_actors_user_id ON rust_actors(user_id);",
    },
    Migration {
        version: 2,
        name: "rust_idempotency",
        sql: "CREATE TABLE rust_idempotency(
                actor_id TEXT NOT NULL,
                method TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(actor_id, method, idempotency_key)
              );
              CREATE INDEX ix_rust_idempotency_actor ON rust_idempotency(actor_id);",
    },
];

#[cfg(test)]
pub(crate) fn ensure_meta_table(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS rust_store_meta(
           key TEXT PRIMARY KEY,
           value TEXT NOT NULL
         );
         COMMIT;",
    )?;
    Ok(())
}

pub(crate) fn table_exists(connection: &Connection, table: &str) -> Result<bool, StoreError> {
    let found: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(crate) fn meta_get(connection: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT value FROM rust_store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

pub(crate) fn meta_set(connection: &Connection, key: &str, value: &str) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO rust_store_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// The current Rust schema version; 0 when no migration has been recorded yet.
pub(crate) fn schema_version(connection: &Connection) -> Result<i64, StoreError> {
    match meta_get(connection, "schema_version")? {
        None => Ok(0),
        Some(value) => value
            .parse()
            .map_err(|_| StoreError::Corruption(format!("invalid rust schema_version {value:?}"))),
    }
}

/// Apply every pending migration, returning the applied versions in order.
pub(crate) fn run_pending(connection: &Connection) -> Result<Vec<i64>, StoreError> {
    run(connection, MIGRATIONS)
}

pub(crate) fn run(
    connection: &Connection,
    migrations: &[Migration],
) -> Result<Vec<i64>, StoreError> {
    let mut version = schema_version(connection)?;
    let latest = migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0);
    if version > latest {
        return Err(StoreError::Corruption(format!(
            "Rust store schema {version} is newer than the supported {latest}"
        )));
    }
    let mut applied = Vec::new();
    for migration in migrations {
        if migration.version <= version {
            continue;
        }
        if migration.version != version + 1 {
            return Err(StoreError::Corruption(format!(
                "migration sequence gap before version {}",
                migration.version
            )));
        }
        let transaction = crate::migrations::immediate_transaction(connection)?;
        transaction.execute_batch(migration.sql).map_err(|error| {
            StoreError::Corruption(format!(
                "migration {} ({}) failed: {error}",
                migration.version, migration.name
            ))
        })?;
        transaction.execute(
            "INSERT INTO rust_store_meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [migration.version.to_string()],
        )?;
        transaction.commit()?;
        version = migration.version;
        applied.push(migration.version);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_connection() -> Connection {
        let connection = Connection::open_in_memory().expect("open");
        ensure_meta_table(&connection).expect("meta");
        connection
            .execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY);")
            .expect("users");
        connection
    }

    #[test]
    fn a_failing_step_rolls_back_and_the_next_run_resumes() {
        static FIRST: Migration = Migration {
            version: 1,
            name: "first",
            sql: "CREATE TABLE step_one(value TEXT);",
        };
        static BROKEN: Migration = Migration {
            version: 2,
            name: "broken",
            sql: "CREATE TABLE step_two(value TEXT); INSERT INTO missing_table VALUES (1);",
        };
        static FIXED: Migration = Migration {
            version: 2,
            name: "fixed",
            sql: "CREATE TABLE step_two(value TEXT);",
        };

        let connection = memory_connection();
        let error = run(&connection, &[FIRST, BROKEN]).expect_err("second step must fail");
        assert!(matches!(error, StoreError::Corruption(message) if message.contains("broken")));
        assert_eq!(schema_version(&connection).expect("version"), 1);
        assert!(table_exists(&connection, "step_one").expect("exists"));
        assert!(!table_exists(&connection, "step_two").expect("exists"));

        let applied = run(&connection, &[FIRST, FIXED]).expect("resume");
        assert_eq!(applied, vec![2]);
        assert_eq!(schema_version(&connection).expect("version"), 2);
        assert!(table_exists(&connection, "step_two").expect("exists"));
    }

    #[test]
    fn a_newer_schema_fails_closed() {
        let connection = memory_connection();
        meta_set(&connection, "schema_version", "99").expect("set");
        let error = run_pending(&connection).expect_err("must fail closed");
        assert!(matches!(error, StoreError::Corruption(message) if message.contains("newer")));
    }
}
