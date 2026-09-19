//! Shared helpers for cool-store integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use cool_store::BASELINE_SCHEMA_SQL;
use rusqlite::Connection;

/// Create a file database that looks exactly like a migrated Python store,
/// with the given extra seed statements applied.
pub fn create_python_database(path: &Path, seed: &str) {
    let connection = Connection::open(path).expect("open fixture database");
    connection
        .execute_batch(BASELINE_SCHEMA_SQL)
        .expect("apply baseline schema");
    if !seed.is_empty() {
        connection.execute_batch(seed).expect("apply seed");
    }
}

/// Default seed: one legacy user (id 1), one conversation and one message.
pub const DEFAULT_SEED: &str = "
INSERT INTO users(created_at, updated_at, id, external_id, username, display_name, is_active)
VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 'local', 'local', 'Local', 1);
INSERT INTO conversations(created_at, updated_at, id, user_id, title, is_pinned, is_archived)
VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 1, 'Legacy chat', 0, 0);
INSERT INTO messages(created_at, updated_at, id, conversation_id, role, content)
VALUES ('2026-01-01 00:00:01.000000', '2026-01-01 00:00:01.000000', 1, 1, 'user', 'hello');
";

pub fn temp_database(directory: &Path, name: &str) -> PathBuf {
    directory.join(name)
}

/// Count rows in a table using a plain rusqlite connection.
pub fn table_count(path: &Path, table: &str) -> i64 {
    let connection = Connection::open(path).expect("open");
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count")
}

/// True when a table exists in the database file.
pub fn has_table(path: &Path, table: &str) -> bool {
    let connection = Connection::open(path).expect("open");
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .expect("query")
        > 0
}
