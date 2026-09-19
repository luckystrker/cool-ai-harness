//! Durable idempotency reservations for protocol mutations (M10).
//!
//! The legacy tables have no idempotency columns, so Rust owns a small
//! `rust_idempotency` table (migration v2). The protocol layer calls
//! [`LegacyStore::run_idempotent`] (or the async variant) with the actor's key
//! and a fingerprint of the request: a replay with the same key and
//! fingerprint returns the stored result instead of repeating the mutation,
//! and a replay with a different fingerprint fails closed with
//! [`StoreError::Conflict`].
//!
//! Execution is reserve → run → complete. A crash after the reservation leaves
//! a `pending` row: the next replay fails closed (`Conflict`) rather than
//! repeating a mutation whose outcome is unknown. This is at-most-once, which
//! is the M10 contract; overwriting pending reservations is not allowed.

use std::future::Future;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::LegacyStore;
use crate::error::StoreError;
use crate::time::now_python;

/// Outcome of an idempotent operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Idempotent<T> {
    pub value: T,
    /// `false` when the stored result of an earlier execution was returned.
    pub created: bool,
}

impl LegacyStore {
    /// Run `action` at most once for `(actor_id, method, idempotency_key)`.
    ///
    /// The action is an ordinary typed store operation; the reservation and
    /// the result marker live in the same database as the mutation.
    pub fn run_idempotent<T, F>(
        &self,
        actor_id: &str,
        method: &str,
        idempotency_key: &str,
        fingerprint: &str,
        action: F,
    ) -> Result<Idempotent<T>, StoreError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Result<T, StoreError>,
    {
        if let Some(existing) =
            self.reserve_idempotent::<T>(actor_id, method, idempotency_key, fingerprint)?
        {
            return Ok(existing);
        }
        let value = action()?;
        self.complete_idempotent(actor_id, method, idempotency_key, &value)?;
        Ok(Idempotent {
            value,
            created: true,
        })
    }

    /// Async variant for actions that must await I/O (for example the workspace
    /// git capability) while keeping the same at-most-once semantics.
    pub async fn run_idempotent_async<T, F, Fut>(
        &self,
        actor_id: &str,
        method: &str,
        idempotency_key: &str,
        fingerprint: &str,
        action: F,
    ) -> Result<Idempotent<T>, StoreError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, StoreError>>,
    {
        if let Some(existing) =
            self.reserve_idempotent::<T>(actor_id, method, idempotency_key, fingerprint)?
        {
            return Ok(existing);
        }
        let value = action().await?;
        self.complete_idempotent(actor_id, method, idempotency_key, &value)?;
        Ok(Idempotent {
            value,
            created: true,
        })
    }

    /// Reserve the key, returning the stored outcome for a replay.
    fn reserve_idempotent<T: DeserializeOwned>(
        &self,
        actor_id: &str,
        method: &str,
        idempotency_key: &str,
        fingerprint: &str,
    ) -> Result<Option<Idempotent<T>>, StoreError> {
        let connection = self.connection()?;
        crate::domains::common::user_id_for(&connection, actor_id)?;
        let existing = connection
            .query_row(
                "SELECT fingerprint, status, result_json FROM rust_idempotency
                 WHERE actor_id = ?1 AND method = ?2 AND idempotency_key = ?3",
                params![actor_id, method, idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_fingerprint, status, result)) = existing {
            if stored_fingerprint != fingerprint {
                return Err(StoreError::Conflict(format!(
                    "idempotency key was reused with different input for {method}"
                )));
            }
            if status != "done" {
                return Err(StoreError::Conflict(format!(
                    "idempotent operation {method} is still pending"
                )));
            }
            let result = result.ok_or_else(|| {
                StoreError::Corruption("idempotency result row is missing".to_owned())
            })?;
            return Ok(Some(Idempotent {
                value: serde_json::from_str(&result)?,
                created: false,
            }));
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO rust_idempotency(actor_id, method, idempotency_key, fingerprint,
               status, result_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', NULL, ?5, ?5)",
            params![actor_id, method, idempotency_key, fingerprint, timestamp],
        )?;
        Ok(None)
    }

    /// Mark a reservation complete with its serialized result.
    fn complete_idempotent<T: Serialize>(
        &self,
        actor_id: &str,
        method: &str,
        idempotency_key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let encoded = serde_json::to_string(value)?;
        let connection = self.connection()?;
        connection.execute(
            "UPDATE rust_idempotency SET status = 'done', result_json = ?1, updated_at = ?2
             WHERE actor_id = ?3 AND method = ?4 AND idempotency_key = ?5",
            params![encoded, now_python(), actor_id, method, idempotency_key],
        )?;
        Ok(())
    }
}
