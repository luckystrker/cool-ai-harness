//! Legacy `providers` store module (M10).
//!
//! Mirrors `backend/app/api/providers.py`: the encrypted API key is passed
//! through opaquely (never decrypted), and `is_default` is mutually exclusive
//! per actor.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{collect_rows, json_text, parse_json, query_one, user_id_for};
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: i64,
    pub name: String,
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub api_key_encrypted: Option<String>,
    pub default_model: Option<String>,
    pub is_active: bool,
    pub is_subscription: bool,
    pub is_fallback: bool,
    pub is_default: bool,
    pub chat_models: Option<Value>,
}

/// Redacts the encrypted API key so provider rows are safe to log.
impl std::fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Provider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field(
                "api_key_encrypted",
                &self.api_key_encrypted.as_ref().map(|_| "[redacted]"),
            )
            .field("default_model", &self.default_model)
            .field("is_active", &self.is_active)
            .field("is_subscription", &self.is_subscription)
            .field("is_fallback", &self.is_fallback)
            .field("is_default", &self.is_default)
            .field("chat_models", &self.chat_models)
            .finish()
    }
}

impl Provider {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            label: row.get("label")?,
            base_url: row.get("base_url")?,
            api_key_encrypted: row.get("api_key_encrypted")?,
            default_model: row.get("default_model")?,
            is_active: row.get::<_, i64>("is_active")? != 0,
            is_subscription: row.get::<_, i64>("is_subscription")? != 0,
            is_fallback: row.get::<_, i64>("is_fallback")? != 0,
            is_default: row.get::<_, i64>("is_default")? != 0,
            chat_models: parse_json(row.get("chat_models")?)?,
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProvider {
    pub name: String,
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub api_key_encrypted: Option<String>,
    pub default_model: Option<String>,
    pub is_active: bool,
    pub is_subscription: bool,
    pub is_fallback: bool,
    pub chat_models: Option<Value>,
}

/// Redacts the encrypted API key so request payloads are safe to log.
impl std::fmt::Debug for NewProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewProvider")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field(
                "api_key_encrypted",
                &self.api_key_encrypted.as_ref().map(|_| "[redacted]"),
            )
            .field("default_model", &self.default_model)
            .field("is_active", &self.is_active)
            .field("is_subscription", &self.is_subscription)
            .field("is_fallback", &self.is_fallback)
            .field("chat_models", &self.chat_models)
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPatch {
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub api_key_encrypted: Option<String>,
    pub default_model: Option<String>,
    pub is_active: Option<bool>,
    pub is_fallback: Option<bool>,
    pub chat_models: Option<Value>,
    pub is_default: Option<bool>,
}

/// Redacts the encrypted API key so request payloads are safe to log.
impl std::fmt::Debug for ProviderPatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderPatch")
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field(
                "api_key_encrypted",
                &self.api_key_encrypted.as_ref().map(|_| "[redacted]"),
            )
            .field("default_model", &self.default_model)
            .field("is_active", &self.is_active)
            .field("is_fallback", &self.is_fallback)
            .field("chat_models", &self.chat_models)
            .field("is_default", &self.is_default)
            .finish()
    }
}

fn fetch_provider(
    connection: &Connection,
    user_id: i64,
    provider_id: i64,
) -> Result<Option<Provider>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM providers WHERE id = ?1 AND user_id = ?2",
        params![provider_id, user_id],
        Provider::from_row,
    )
}

impl crate::LegacyStore {
    /// Providers for the actor, optionally including inactive rows.
    pub fn list_providers(
        &self,
        actor_id: &str,
        include_inactive: bool,
    ) -> Result<Vec<Provider>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM providers WHERE user_id = ?1 AND (?2 = 1 OR is_active = 1) \
             ORDER BY id",
        )?;
        let rows = statement.query(params![user_id, i64::from(include_inactive)])?;
        collect_rows(rows, Provider::from_row)
    }

    pub fn get_provider(&self, actor_id: &str, provider_id: i64) -> Result<Provider, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_provider(&connection, user_id, provider_id)?.ok_or(StoreError::NotFound("provider"))
    }

    pub fn create_provider(
        &self,
        actor_id: &str,
        new: &NewProvider,
    ) -> Result<Provider, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO providers(created_at, updated_at, user_id, name, label, base_url,
               api_key_encrypted, default_model, is_active, is_subscription, is_fallback,
               is_default, chat_models)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)",
            params![
                timestamp,
                user_id,
                new.name,
                new.label,
                new.base_url,
                new.api_key_encrypted,
                new.default_model,
                i64::from(new.is_active),
                i64::from(new.is_subscription),
                i64::from(new.is_fallback),
                json_text(&new.chat_models)?,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_provider(actor_id, id)
    }

    pub fn update_provider(
        &self,
        actor_id: &str,
        provider_id: i64,
        patch: &ProviderPatch,
    ) -> Result<Provider, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_provider(&connection, user_id, provider_id)?
            .ok_or(StoreError::NotFound("provider"))?;
        if patch.is_default == Some(true) {
            connection.execute(
                "UPDATE providers SET is_default = 0, updated_at = ?1 WHERE user_id = ?2 AND id != ?3",
                params![now_python(), user_id, provider_id],
            )?;
        }
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(label) = &patch.label {
            assignments.push("label = ?");
            values.push(Box::new(label.clone()));
        }
        if let Some(base_url) = &patch.base_url {
            assignments.push("base_url = ?");
            values.push(Box::new(base_url.clone()));
        }
        if let Some(key) = &patch.api_key_encrypted {
            assignments.push("api_key_encrypted = ?");
            values.push(Box::new(key.clone()));
        }
        if let Some(model) = &patch.default_model {
            assignments.push("default_model = ?");
            values.push(Box::new(model.clone()));
        }
        if let Some(active) = patch.is_active {
            assignments.push("is_active = ?");
            values.push(Box::new(i64::from(active)));
        }
        if let Some(fallback) = patch.is_fallback {
            assignments.push("is_fallback = ?");
            values.push(Box::new(i64::from(fallback)));
        }
        if let Some(chat_models) = &patch.chat_models {
            assignments.push("chat_models = ?");
            values.push(Box::new(json_text(&Some(chat_models.clone()))?));
        }
        if let Some(is_default) = patch.is_default {
            assignments.push("is_default = ?");
            values.push(Box::new(i64::from(is_default)));
        }
        assignments.push("updated_at = ?");
        values.push(Box::new(now_python()));
        values.push(Box::new(provider_id));
        values.push(Box::new(user_id));
        let sql = format!(
            "UPDATE providers SET {} WHERE id = ? AND user_id = ?",
            assignments.join(", ")
        );
        let references: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|value| value.as_ref()).collect();
        connection.execute(&sql, references.as_slice())?;
        drop(connection);
        self.get_provider(actor_id, provider_id)
    }

    pub fn delete_provider(&self, actor_id: &str, provider_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_provider(&connection, user_id, provider_id)?
            .ok_or(StoreError::NotFound("provider"))?;
        connection.execute(
            "DELETE FROM providers WHERE id = ?1 AND user_id = ?2",
            params![provider_id, user_id],
        )?;
        Ok(())
    }

    /// Mark `provider_id` as the actor's only default provider.
    pub fn set_default_provider(
        &self,
        actor_id: &str,
        provider_id: i64,
    ) -> Result<Provider, StoreError> {
        let mut connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_provider(&connection, user_id, provider_id)?
            .ok_or(StoreError::NotFound("provider"))?;
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE providers SET is_default = 0, updated_at = ?1 WHERE user_id = ?2 AND id != ?3",
            params![timestamp, user_id, provider_id],
        )?;
        transaction.execute(
            "UPDATE providers SET is_default = 1, updated_at = ?1 WHERE id = ?2 AND user_id = ?3",
            params![timestamp, provider_id, user_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_provider(actor_id, provider_id)
    }

    /// Default provider for new conversations, matching
    /// `resolve_default_model` selection order (default non-fallback, else the
    /// first active non-fallback provider by id).
    pub fn default_provider(&self, actor_id: &str) -> Result<Option<Provider>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection
            .prepare("SELECT * FROM providers WHERE user_id = ?1 AND is_active = 1 ORDER BY id")?;
        let rows = statement.query([user_id])?;
        let providers = collect_rows(rows, Provider::from_row)?;
        if let Some(default) = providers
            .iter()
            .find(|provider| provider.is_default && !provider.is_fallback)
        {
            return Ok(Some(default.clone()));
        }
        Ok(providers.into_iter().find(|provider| !provider.is_fallback))
    }
}
