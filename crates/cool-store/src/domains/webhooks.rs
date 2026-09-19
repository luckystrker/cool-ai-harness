//! Webhook endpoints and events (legacy `webhook_endpoints` / `webhook_events`).
//!
//! Mirrors `backend/app/webhooks/service.py`. Endpoint management is
//! actor-scoped. The inbound receive path (`find_endpoint_by_hook_id` and
//! `record_webhook_event`) is deliberately **not** actor-scoped: an external
//! system authenticates with the unguessable `hook_id` plus the HMAC secret and
//! has no local actor identity, so these two methods are documented exceptions
//! to the actor rule.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

/// Valid `source_type` values, mirroring `app.models.webhook.SOURCE_TYPES`.
pub const SOURCE_TYPES: &[&str] = &["github", "notion", "slack", "custom"];
/// Valid event processing statuses, mirroring `EVENT_STATUSES`.
pub const EVENT_STATUSES: &[&str] = &["received", "processing", "completed", "failed", "rejected"];

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookEndpoint {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub hook_id: String,
    pub secret: String,
    pub source_type: String,
    pub event_filter: Option<Value>,
    pub task_id: Option<i64>,
    pub prompt_template: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl std::fmt::Debug for WebhookEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookEndpoint")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("name", &self.name)
            .field("hook_id", &self.hook_id)
            .field("secret", &"[redacted]")
            .field("source_type", &self.source_type)
            .field("event_filter", &self.event_filter)
            .field("task_id", &self.task_id)
            .field("prompt_template", &self.prompt_template)
            .field("enabled", &self.enabled)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl WebhookEndpoint {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            name: row.get("name")?,
            hook_id: row.get("hook_id")?,
            secret: row.get("secret")?,
            source_type: row.get("source_type")?,
            event_filter: parse_json(row.get("event_filter")?)?,
            task_id: row.get("task_id")?,
            prompt_template: row.get("prompt_template")?,
            enabled: row.get::<_, i64>("enabled")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWebhookEndpoint {
    pub name: String,
    /// Generated as a hyphenless UUID v4 when absent (Python `uuid.uuid4().hex`).
    pub hook_id: Option<String>,
    /// Generated as 32 random bytes hex-encoded when absent.
    pub secret: Option<String>,
    pub source_type: String,
    pub event_filter: Option<Value>,
    pub task_id: Option<i64>,
    pub prompt_template: Option<String>,
    pub enabled: Option<bool>,
}

/// Redacts the HMAC secret so request payloads are safe to log.
impl std::fmt::Debug for NewWebhookEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewWebhookEndpoint")
            .field("name", &self.name)
            .field("hook_id", &self.hook_id)
            .field("secret", &self.secret.as_ref().map(|_| "[redacted]"))
            .field("source_type", &self.source_type)
            .field("event_filter", &self.event_filter)
            .field("task_id", &self.task_id)
            .field("prompt_template", &self.prompt_template)
            .field("enabled", &self.enabled)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookEndpointPatch {
    pub name: Option<String>,
    pub source_type: Option<String>,
    pub event_filter: Option<Value>,
    pub task_id: Option<i64>,
    pub prompt_template: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookEvent {
    pub id: i64,
    pub endpoint_id: i64,
    pub event_type: Option<String>,
    pub payload: Option<Value>,
    pub signature_valid: bool,
    pub status: String,
    pub task_run_id: Option<i64>,
    pub error: Option<String>,
    pub received_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl WebhookEvent {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            endpoint_id: row.get("endpoint_id")?,
            event_type: row.get("event_type")?,
            payload: parse_json(row.get("payload")?)?,
            signature_valid: row.get::<_, i64>("signature_valid")? != 0,
            status: row.get("status")?,
            task_run_id: row.get("task_run_id")?,
            error: row.get("error")?,
            received_at: row.get("received_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Inbound event payload. `status` borrows because callers pass a literal
/// (`Some("received")`); it defaults to `"received"` when absent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWebhookEvent<'a> {
    pub event_type: Option<String>,
    pub payload: Option<Value>,
    pub signature_valid: bool,
    #[serde(borrow)]
    pub status: Option<&'a str>,
}

fn random_hook_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn random_secret() -> String {
    // 32 random bytes hex-encoded, matching Python `secrets.token_hex(32)`.
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn validate_source_type(source_type: &str) -> Result<(), StoreError> {
    if SOURCE_TYPES.contains(&source_type) {
        Ok(())
    } else {
        Err(StoreError::InvalidInput(format!(
            "unknown source_type {source_type:?} (expected one of {SOURCE_TYPES:?})"
        )))
    }
}

/// Fetch an endpoint while validating actor ownership.
fn fetch_endpoint(
    connection: &Connection,
    actor_id: &str,
    endpoint_id: i64,
) -> Result<WebhookEndpoint, StoreError> {
    let user_id = user_id_for(connection, actor_id)?;
    query_one(
        connection,
        "SELECT * FROM webhook_endpoints WHERE id = ?1 AND user_id = ?2",
        params![endpoint_id, user_id],
        WebhookEndpoint::from_row,
    )?
    .ok_or(StoreError::NotFound("webhook endpoint"))
}

/// Fetch an event whose endpoint belongs to the actor.
fn fetch_event_owned(
    connection: &Connection,
    actor_id: &str,
    event_id: i64,
) -> Result<WebhookEvent, StoreError> {
    let user_id = user_id_for(connection, actor_id)?;
    query_one(
        connection,
        "SELECT e.* FROM webhook_events e JOIN webhook_endpoints w ON w.id = e.endpoint_id \
         WHERE e.id = ?1 AND w.user_id = ?2",
        params![event_id, user_id],
        WebhookEvent::from_row,
    )?
    .ok_or(StoreError::NotFound("webhook event"))
}

impl crate::LegacyStore {
    /// Endpoints for the actor, newest first (Python orders by `id DESC`).
    pub fn list_endpoints(&self, actor_id: &str) -> Result<Vec<WebhookEndpoint>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection
            .prepare("SELECT * FROM webhook_endpoints WHERE user_id = ?1 ORDER BY id DESC")?;
        let rows = statement.query([user_id])?;
        collect_rows(rows, WebhookEndpoint::from_row)
    }

    pub fn get_endpoint(
        &self,
        actor_id: &str,
        endpoint_id: i64,
    ) -> Result<WebhookEndpoint, StoreError> {
        let connection = self.connection()?;
        fetch_endpoint(&connection, actor_id, endpoint_id)
    }

    pub fn create_endpoint(
        &self,
        actor_id: &str,
        new: &NewWebhookEndpoint,
    ) -> Result<WebhookEndpoint, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        validate_source_type(&new.source_type)?;
        let hook_id = new.hook_id.clone().unwrap_or_else(random_hook_id);
        let secret = new.secret.clone().unwrap_or_else(random_secret);
        let enabled = new.enabled.unwrap_or(true);
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO webhook_endpoints(created_at, updated_at, user_id, name, hook_id, secret,
               source_type, event_filter, task_id, prompt_template, enabled)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                timestamp,
                user_id,
                new.name,
                hook_id,
                secret,
                new.source_type,
                json_text(&new.event_filter)?,
                new.task_id,
                new.prompt_template,
                i64::from(enabled),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_endpoint(actor_id, id)
    }

    /// Delete an endpoint and its event history (Python `delete_endpoint`).
    pub fn delete_endpoint(&self, actor_id: &str, endpoint_id: i64) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        fetch_endpoint(&connection, actor_id, endpoint_id)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM webhook_events WHERE endpoint_id = ?1",
            [endpoint_id],
        )?;
        transaction.execute("DELETE FROM webhook_endpoints WHERE id = ?1", [endpoint_id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Patch an endpoint. `None` fields are left unchanged; `Some(Value::Null)`
    /// clears a JSON column. `hook_id`/`secret` are never patched, matching
    /// Python's allow-list.
    pub fn update_endpoint(
        &self,
        actor_id: &str,
        endpoint_id: i64,
        patch: &WebhookEndpointPatch,
    ) -> Result<WebhookEndpoint, StoreError> {
        let connection = self.connection()?;
        fetch_endpoint(&connection, actor_id, endpoint_id)?;
        if let Some(source_type) = &patch.source_type {
            validate_source_type(source_type)?;
        }
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(name) = &patch.name {
            assignments.push("name = ?");
            values.push(Box::new(name.clone()));
        }
        if let Some(source_type) = &patch.source_type {
            assignments.push("source_type = ?");
            values.push(Box::new(source_type.clone()));
        }
        if patch.event_filter.is_some() {
            assignments.push("event_filter = ?");
            values.push(Box::new(json_text(&patch.event_filter)?));
        }
        if patch.task_id.is_some() {
            assignments.push("task_id = ?");
            values.push(Box::new(patch.task_id));
        }
        if patch.prompt_template.is_some() {
            assignments.push("prompt_template = ?");
            values.push(Box::new(patch.prompt_template.clone()));
        }
        if let Some(enabled) = patch.enabled {
            assignments.push("enabled = ?");
            values.push(Box::new(i64::from(enabled)));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(endpoint_id));
            let sql = format!(
                "UPDATE webhook_endpoints SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_endpoint(actor_id, endpoint_id)
    }

    /// Public inbound path: look up an endpoint by its unguessable `hook_id`.
    ///
    /// Documented exception to actor scoping — external systems have no local
    /// actor and authenticate through the hook id plus HMAC.
    pub fn find_endpoint_by_hook_id(
        &self,
        hook_id: &str,
    ) -> Result<Option<WebhookEndpoint>, StoreError> {
        let connection = self.connection()?;
        query_one(
            &connection,
            "SELECT * FROM webhook_endpoints WHERE hook_id = ?1",
            [hook_id],
            WebhookEndpoint::from_row,
        )
    }

    /// Public inbound path: record a received event.
    ///
    /// Documented exception to actor scoping (see module docs). `status`
    /// defaults to `"received"`.
    pub fn record_webhook_event(
        &self,
        endpoint_id: i64,
        new: &NewWebhookEvent<'_>,
    ) -> Result<WebhookEvent, StoreError> {
        let connection = self.connection()?;
        let exists: Option<i64> = connection
            .query_row(
                "SELECT id FROM webhook_endpoints WHERE id = ?1",
                [endpoint_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound("webhook endpoint"));
        }
        let timestamp = now_python();
        let status = new.status.unwrap_or("received");
        connection.execute(
            "INSERT INTO webhook_events(created_at, updated_at, endpoint_id, event_type, payload,
               signature_valid, status, received_at)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?1)",
            params![
                timestamp,
                endpoint_id,
                new.event_type,
                json_text(&new.payload)?,
                i64::from(new.signature_valid),
                status,
            ],
        )?;
        let id = connection.last_insert_rowid();
        query_one(
            &connection,
            "SELECT * FROM webhook_events WHERE id = ?1",
            [id],
            WebhookEvent::from_row,
        )?
        .ok_or(StoreError::NotFound("webhook event"))
    }

    /// Public inbound path: update a recorded event's processing outcome.
    ///
    /// Documented exception to actor scoping (see module docs).
    pub fn update_webhook_event(
        &self,
        event_id: i64,
        status: &str,
        error: Option<&str>,
        task_run_id: Option<i64>,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let updated = connection.execute(
            "UPDATE webhook_events SET status = ?1, error = ?2, task_run_id = ?3, updated_at = ?4 \
             WHERE id = ?5",
            params![status, error, task_run_id, now_python(), event_id],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound("webhook event"));
        }
        Ok(())
    }

    /// Event history for one owned endpoint, newest first.
    pub fn list_webhook_events(
        &self,
        actor_id: &str,
        endpoint_id: i64,
        limit: Option<usize>,
    ) -> Result<Vec<WebhookEvent>, StoreError> {
        let connection = self.connection()?;
        fetch_endpoint(&connection, actor_id, endpoint_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM webhook_events WHERE endpoint_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query(params![endpoint_id, bounded_limit(limit, 50, 200)])?;
        collect_rows(rows, WebhookEvent::from_row)
    }

    /// Owner-scoped event read (used by tests and diagnostics).
    pub fn get_webhook_event(
        &self,
        actor_id: &str,
        event_id: i64,
    ) -> Result<WebhookEvent, StoreError> {
        let connection = self.connection()?;
        fetch_event_owned(&connection, actor_id, event_id)
    }
}
