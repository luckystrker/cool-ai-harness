//! Actor ↔ legacy `users.id` mapping.
//!
//! The Python schema identifies rows by integer `user_id`; the Rust protocol
//! identifies callers by an opaque actor id. `rust_actors` binds the two so the
//! protocol layer never falls back to an implicit `user_id = 1`.

use rusqlite::{Connection, OptionalExtension};

use crate::error::StoreError;
use crate::time::now_python;

/// Single-user local default used by the CLI and the Web facade.
pub const DEFAULT_ACTOR_ID: &str = "local-user";

/// Resolve an actor to its legacy `users.id`.
///
/// Adopted stores resolve through `rust_actors`. A database that was only
/// opened read-only (a Python store Rust has not adopted yet) has no mapping
/// table, so the actor is matched against `users.external_id`/`username`; the
/// single-user local default falls back to the first legacy user.
pub(crate) fn resolve_user(connection: &Connection, actor_id: &str) -> Result<i64, StoreError> {
    if crate::migrations::table_exists(connection, "rust_actors")? {
        let mapped: Option<i64> = connection
            .query_row(
                "SELECT user_id FROM rust_actors WHERE actor_id = ?1",
                [actor_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(user_id) = mapped {
            return Ok(user_id);
        }
    }
    let matched: Option<i64> = connection
        .query_row(
            "SELECT id FROM users WHERE external_id = ?1 OR username = ?1 ORDER BY id LIMIT 1",
            [actor_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(user_id) = matched {
        return Ok(user_id);
    }
    if actor_id == DEFAULT_ACTOR_ID {
        let first: Option<i64> = connection
            .query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(user_id) = first {
            return Ok(user_id);
        }
    }
    Err(StoreError::ActorUnknown(actor_id.to_string()))
}

/// Register `actor_id` if needed.
///
/// The local default actor reuses (or creates) `users.id = 1`; any other actor
/// gets a fresh user row whose `external_id` is the actor id.
pub(crate) fn ensure_actor(connection: &Connection, actor_id: &str) -> Result<i64, StoreError> {
    if let Some(user_id) = connection
        .query_row(
            "SELECT user_id FROM rust_actors WHERE actor_id = ?1",
            [actor_id],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(user_id);
    }
    let timestamp = now_python();
    let user_id = if actor_id == DEFAULT_ACTOR_ID {
        match connection
            .query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
        {
            Some(existing) => existing,
            None => {
                connection.execute(
                    "INSERT INTO users(created_at, updated_at, external_id, username,
                       display_name, is_active)
                     VALUES (?1, ?1, 'local', 'local', 'Local user', 1)",
                    [&timestamp],
                )?;
                connection.last_insert_rowid()
            }
        }
    } else {
        connection.execute(
            "INSERT INTO users(created_at, updated_at, external_id, username,
               display_name, is_active)
             VALUES (?1, ?1, ?2, ?2, ?2, 1)",
            rusqlite::params![&timestamp, actor_id],
        )?;
        connection.last_insert_rowid()
    };
    connection.execute(
        "INSERT INTO rust_actors(actor_id, user_id, display_name, created_at)
         VALUES (?1, ?2, NULL, ?3)",
        rusqlite::params![actor_id, user_id, timestamp],
    )?;
    Ok(user_id)
}

/// The actor bound to a legacy `users.id`, when one exists.
pub fn actor_for_user(connection: &Connection, user_id: i64) -> Result<Option<String>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT actor_id FROM rust_actors WHERE user_id = ?1",
            [user_id],
            |row| row.get(0),
        )
        .optional()?)
}
