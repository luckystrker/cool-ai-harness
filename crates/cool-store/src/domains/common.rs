//! Shared helpers for the legacy schema domain modules.

use rusqlite::{Connection, OptionalExtension, Row, Rows};
use serde_json::Value;

use crate::actors::resolve_user;
use crate::error::StoreError;

/// Iterate a `Rows` cursor with a fallible domain mapper.
pub(crate) fn collect_rows<T>(
    mut rows: Rows<'_>,
    mapper: impl Fn(&Row<'_>) -> Result<T, StoreError>,
) -> Result<Vec<T>, StoreError> {
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        values.push(mapper(row)?);
    }
    Ok(values)
}

/// `query_row` with a domain mapper that returns [`StoreError`].
pub(crate) fn query_one<T>(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    mapper: impl FnOnce(&Row<'_>) -> Result<T, StoreError>,
) -> Result<Option<T>, StoreError> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(params)?;
    match rows.next()? {
        Some(row) => Ok(Some(mapper(row)?)),
        None => Ok(None),
    }
}

/// Resolve an actor to `users.id`, failing closed for unknown actors.
pub(crate) fn user_id_for(connection: &Connection, actor_id: &str) -> Result<i64, StoreError> {
    resolve_user(connection, actor_id)
}

/// Parse an optional JSON column into a value.
pub(crate) fn parse_json(value: Option<String>) -> Result<Option<Value>, StoreError> {
    match value {
        None => Ok(None),
        Some(text) if text.is_empty() => Ok(None),
        Some(text) => Ok(Some(serde_json::from_str(&text)?)),
    }
}

/// Serialize an optional JSON column for storage.
pub(crate) fn json_text(value: &Option<Value>) -> Result<Option<String>, StoreError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(serde_json::to_string(value)?)),
    }
}

/// Ensure a bounded limit without silently accepting zero.
pub(crate) fn bounded_limit(limit: Option<usize>, default: usize, maximum: usize) -> i64 {
    let value = limit.unwrap_or(default).clamp(1, maximum);
    value as i64
}

/// Fetch the legacy `users.id` that owns a conversation.
pub(crate) fn conversation_owner(
    connection: &Connection,
    conversation_id: i64,
) -> Result<Option<i64>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT user_id FROM conversations WHERE id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Require that `actor_id` owns `conversation_id`.
pub(crate) fn require_conversation(
    connection: &Connection,
    actor_id: &str,
    conversation_id: i64,
) -> Result<i64, StoreError> {
    let user_id = user_id_for(connection, actor_id)?;
    let owner = conversation_owner(connection, conversation_id)?
        .ok_or(StoreError::NotFound("conversation"))?;
    if owner != user_id {
        return Err(StoreError::NotFound("conversation"));
    }
    Ok(user_id)
}
