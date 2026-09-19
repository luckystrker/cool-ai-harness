//! Legacy `memory` store module (M10).
//!
//! Typed access to the long-term memory tables introduced in Phase 3a:
//! `memory_items`, `episodes`, `working_memory`, `entities`,
//! `entity_relations`, `memory_item_entities` and `memory_embeddings`.
//!
//! ## Scoping
//!
//! Every table is user-scoped (`user_id`) and the Rust actor is resolved to the
//! legacy `users.id` through [`crate::domains::common::user_id_for`]. Memory
//! visibility is *not* keyed by a stored `project_key` column — the
//! `memory_items` table has no such column. Visibility is the tuple
//! `(user_id, scope, conversation_id)`:
//!
//! - `scope = "global"`  → visible to every conversation of the user.
//! - `scope = "agent"`   → visible only when the active `agent_id` matches.
//! - `scope = "conversation"` → visible inside the originating conversation.
//!
//! The Python runtime additionally stores the originating conversation's
//! working directory as `structured["_project_key"]`, which lets a
//! conversation-scoped memory surface in *other* conversations of the same
//! project. Rust mirrors that convention when creating conversation-scoped
//! memories and when applying visibility in [`crate::memory`].
//!
//! ## Connection discipline
//!
//! None of these methods may call another `&self` method while holding
//! `self.connection()` — the connection mutex is not reentrant. Free `fetch_*`
//! helpers take the held guard instead.

use std::collections::BTreeMap;

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, require_conversation,
    user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

// --- Status / type / scope constants (mirror app/memory/models.py) ---

pub const MEMORY_STATUS_ACTIVE: &str = "active";
pub const MEMORY_STATUS_ARCHIVED: &str = "archived";
pub const MEMORY_STATUS_SUPERSEDED: &str = "superseded";
pub const MEMORY_STATUS_DELETED: &str = "deleted";
pub const MEMORY_STATUS_PENDING_CONFIRMATION: &str = "pending_confirmation";

pub const MEMORY_TYPE_SEMANTIC: &str = "semantic";
pub const MEMORY_TYPE_EPISODIC: &str = "episodic";
pub const MEMORY_TYPE_PROCEDURAL: &str = "procedural";
pub const MEMORY_TYPE_PREFERENCE: &str = "preference";

pub const SCOPE_GLOBAL: &str = "global";
pub const SCOPE_AGENT: &str = "agent";
pub const SCOPE_CONVERSATION: &str = "conversation";

/// Maximum importance an agent-sourced write may carry without confirmation.
pub const MAX_AGENT_IMPORTANCE: f64 = 0.9;
/// Default embedding dimension (mirrors `Settings.memory_embedding_dim`).
pub const MEMORY_EMBEDDING_DIM: i64 = 1536;

/// Embedding dimension used by the `memory_vec` index (Python default 1536).
pub const fn memory_embedding_dimension() -> i64 {
    MEMORY_EMBEDDING_DIM
}

// --- Memory items ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryItem {
    pub id: i64,
    pub user_id: i64,
    pub scope: String,
    pub agent_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub memory_type: String,
    pub content: String,
    pub structured: Option<Value>,
    pub tags: Option<Value>,
    pub importance: f64,
    pub confidence: f64,
    pub source: String,
    pub status: String,
    pub supersedes_id: Option<i64>,
    pub access_count: i64,
    pub last_accessed_at: Option<String>,
    pub ttl_days: Option<i64>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl MemoryItem {
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            scope: row.get("scope")?,
            agent_id: row.get("agent_id")?,
            conversation_id: row.get("conversation_id")?,
            memory_type: row.get("memory_type")?,
            content: row.get("content")?,
            structured: parse_json(row.get("structured")?)?,
            tags: parse_json(row.get("tags")?)?,
            importance: row.get("importance")?,
            confidence: row.get("confidence")?,
            source: row.get("source")?,
            status: row.get("status")?,
            supersedes_id: row.get("supersedes_id")?,
            access_count: row.get("access_count")?,
            last_accessed_at: row.get("last_accessed_at")?,
            ttl_days: row.get("ttl_days")?,
            valid_from: row.get("valid_from")?,
            valid_to: row.get("valid_to")?,
            pinned: row.get::<_, i64>("pinned")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMemoryItem {
    pub scope: String,
    pub agent_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub memory_type: String,
    pub content: String,
    pub structured: Option<Value>,
    pub tags: Option<Value>,
    pub importance: f64,
    pub confidence: f64,
    pub source: String,
    /// Explicit status override. When `None`, trusted sources
    /// (`user_explicit`, `system`) and `confirmed` writes become `active`;
    /// everything else lands in `pending_confirmation`.
    pub status: Option<String>,
    pub confirmed: bool,
    pub supersedes_id: Option<i64>,
    pub ttl_days: Option<i64>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub pinned: bool,
}

impl Default for NewMemoryItem {
    fn default() -> Self {
        Self {
            scope: SCOPE_GLOBAL.to_string(),
            agent_id: None,
            conversation_id: None,
            memory_type: MEMORY_TYPE_SEMANTIC.to_string(),
            content: String::new(),
            structured: None,
            tags: None,
            importance: 0.5,
            confidence: 0.7,
            source: "agent".to_string(),
            status: None,
            confirmed: false,
            supersedes_id: None,
            ttl_days: None,
            valid_from: None,
            valid_to: None,
            pinned: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryItemPatch {
    pub content: Option<String>,
    pub memory_type: Option<String>,
    pub scope: Option<String>,
    pub agent_id: Option<i64>,
    pub importance: Option<f64>,
    pub confidence: Option<f64>,
    pub status: Option<String>,
    pub tags: Option<Value>,
    pub structured: Option<Value>,
    pub ttl_days: Option<i64>,
    pub valid_to: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct MemoryFilter {
    pub memory_type: Option<String>,
    pub scope: Option<String>,
    /// `None` keeps every status (pass `Some("active")` for the Python API
    /// default filter).
    pub status: Option<String>,
    pub conversation_id: Option<i64>,
    pub pinned: Option<bool>,
    pub limit: Option<usize>,
    pub offset: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    pub total_active: i64,
    pub by_type: BTreeMap<String, i64>,
    pub by_scope: BTreeMap<String, i64>,
    pub total_episodes: i64,
    pub total_archived: i64,
    pub total_pending: i64,
    pub total_entities: i64,
}

fn default_memory_status(source: &str, confirmed: bool) -> &'static str {
    if confirmed || matches!(source, "user_explicit" | "system") {
        MEMORY_STATUS_ACTIVE
    } else {
        MEMORY_STATUS_PENDING_CONFIRMATION
    }
}

/// Fetch a user-owned memory item without re-locking the store.
fn fetch_memory_item(
    connection: &Connection,
    user_id: i64,
    memory_id: i64,
) -> Result<Option<MemoryItem>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM memory_items WHERE id = ?1 AND user_id = ?2",
        params![memory_id, user_id],
        MemoryItem::from_row,
    )
}

fn require_memory_item(
    connection: &Connection,
    user_id: i64,
    memory_id: i64,
) -> Result<MemoryItem, StoreError> {
    fetch_memory_item(connection, user_id, memory_id)?.ok_or(StoreError::NotFound("memory item"))
}

/// Attach the conversation's working directory as `_project_key` for
/// conversation-scoped memories (mirrors `service._attach_project_key`).
fn attach_project_key(
    connection: &Connection,
    scope: &str,
    conversation_id: Option<i64>,
    structured: Option<Value>,
) -> Result<Option<Value>, StoreError> {
    if scope != SCOPE_CONVERSATION {
        return Ok(structured);
    }
    let Some(conversation_id) = conversation_id else {
        return Ok(structured);
    };
    let workdir: Option<String> = connection
        .query_row(
            "SELECT working_directory FROM conversations WHERE id = ?1",
            [conversation_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    let mut map = match structured {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    if let Some(directory) = workdir
        && !directory.is_empty()
    {
        map.insert("_project_key".to_string(), Value::String(directory));
    }
    if map.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(map)))
    }
}

impl crate::LegacyStore {
    pub fn list_memory_items(
        &self,
        actor_id: &str,
        filter: &MemoryFilter,
    ) -> Result<Vec<MemoryItem>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM memory_items WHERE user_id = ?1 \
             AND (?2 IS NULL OR memory_type = ?2) \
             AND (?3 IS NULL OR scope = ?3) \
             AND (?4 IS NULL OR status = ?4) \
             AND (?5 IS NULL OR conversation_id = ?5) \
             AND (?6 IS NULL OR pinned = ?6) \
             ORDER BY importance DESC, updated_at DESC \
             LIMIT ?7 OFFSET ?8",
        )?;
        let rows = statement.query(params![
            user_id,
            filter.memory_type,
            filter.scope,
            filter.status,
            filter.conversation_id,
            filter.pinned.map(i64::from),
            bounded_limit(filter.limit, 100, 500),
            filter.offset as i64,
        ])?;
        collect_rows(rows, MemoryItem::from_row)
    }

    pub fn get_memory_item(
        &self,
        actor_id: &str,
        memory_id: i64,
    ) -> Result<MemoryItem, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)
    }

    pub fn create_memory_item(
        &self,
        actor_id: &str,
        new: &NewMemoryItem,
    ) -> Result<MemoryItem, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(conversation_id) = new.conversation_id {
            require_conversation(&connection, actor_id, conversation_id)?;
        }
        let mut importance = new.importance.clamp(0.0, 1.0);
        if new.source != "user_explicit" && importance > MAX_AGENT_IMPORTANCE {
            importance = MAX_AGENT_IMPORTANCE;
        }
        let confidence = new.confidence.clamp(0.0, 1.0);
        let status = new
            .status
            .clone()
            .unwrap_or_else(|| default_memory_status(&new.source, new.confirmed).to_string());
        let structured = attach_project_key(
            &connection,
            &new.scope,
            new.conversation_id,
            new.structured.clone(),
        )?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO memory_items(created_at, updated_at, user_id, scope, agent_id,
               conversation_id, memory_type, content, structured, tags, importance, confidence,
               source, status, supersedes_id, access_count, last_accessed_at, ttl_days,
               valid_from, valid_to, pinned)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0,
               NULL, ?15, ?16, ?17, ?18)",
            params![
                timestamp,
                user_id,
                new.scope,
                new.agent_id,
                new.conversation_id,
                new.memory_type,
                new.content,
                json_text(&structured)?,
                json_text(&new.tags)?,
                importance,
                confidence,
                new.source,
                status,
                new.supersedes_id,
                new.ttl_days,
                new.valid_from,
                new.valid_to,
                i64::from(new.pinned),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_memory_item(actor_id, id)
    }

    pub fn update_memory_item(
        &self,
        actor_id: &str,
        memory_id: i64,
        patch: &MemoryItemPatch,
    ) -> Result<MemoryItem, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(content) = &patch.content {
            assignments.push("content = ?");
            values.push(Box::new(content.clone()));
        }
        if let Some(memory_type) = &patch.memory_type {
            assignments.push("memory_type = ?");
            values.push(Box::new(memory_type.clone()));
        }
        if let Some(scope) = &patch.scope {
            assignments.push("scope = ?");
            values.push(Box::new(scope.clone()));
        }
        if let Some(agent_id) = patch.agent_id {
            assignments.push("agent_id = ?");
            values.push(Box::new(agent_id));
        }
        if let Some(importance) = patch.importance {
            assignments.push("importance = ?");
            values.push(Box::new(importance.clamp(0.0, 1.0)));
        }
        if let Some(confidence) = patch.confidence {
            assignments.push("confidence = ?");
            values.push(Box::new(confidence.clamp(0.0, 1.0)));
        }
        if let Some(status) = &patch.status {
            assignments.push("status = ?");
            values.push(Box::new(status.clone()));
        }
        if patch.tags.is_some() {
            assignments.push("tags = ?");
            values.push(Box::new(json_text(&patch.tags)?));
        }
        if patch.structured.is_some() {
            assignments.push("structured = ?");
            values.push(Box::new(json_text(&patch.structured)?));
        }
        if let Some(ttl_days) = patch.ttl_days {
            assignments.push("ttl_days = ?");
            values.push(Box::new(ttl_days));
        }
        if let Some(valid_to) = &patch.valid_to {
            assignments.push("valid_to = ?");
            values.push(Box::new(valid_to.clone()));
        }
        if let Some(pinned) = patch.pinned {
            assignments.push("pinned = ?");
            values.push(Box::new(i64::from(pinned)));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(memory_id));
            let sql = format!(
                "UPDATE memory_items SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_memory_item(actor_id, memory_id)
    }

    /// Soft-delete (archive) by default; `hard = true` removes the row and its
    /// link/embedding bookkeeping. The `memory_items_ad` trigger maintains the
    /// FTS index on hard delete.
    pub fn delete_memory_item(
        &self,
        actor_id: &str,
        memory_id: i64,
        hard: bool,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        if hard {
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM memory_item_entities WHERE memory_id = ?1",
                [memory_id],
            )?;
            transaction.execute(
                "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                [memory_id],
            )?;
            transaction.execute("DELETE FROM memory_items WHERE id = ?1", [memory_id])?;
            transaction.commit()?;
        } else {
            connection.execute(
                "UPDATE memory_items SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![MEMORY_STATUS_ARCHIVED, now_python(), memory_id],
            )?;
        }
        Ok(())
    }

    /// Promote a pending memory to `active`, archiving the memory it supersedes.
    pub fn confirm_memory_item(
        &self,
        actor_id: &str,
        memory_id: i64,
    ) -> Result<MemoryItem, StoreError> {
        let mut connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let memory = require_memory_item(&connection, user_id, memory_id)?;
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE memory_items SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![MEMORY_STATUS_ACTIVE, timestamp, memory_id],
        )?;
        if let Some(superseded_id) = memory.supersedes_id
            && superseded_id != memory_id
        {
            transaction.execute(
                "UPDATE memory_items SET status = ?1, supersedes_id = ?2, updated_at = ?3 \
                 WHERE id = ?4 AND user_id = ?5 AND status = ?6",
                params![
                    MEMORY_STATUS_SUPERSEDED,
                    memory_id,
                    timestamp,
                    superseded_id,
                    user_id,
                    MEMORY_STATUS_ACTIVE,
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.get_memory_item(actor_id, memory_id)
    }

    /// Archive a pending memory (reject).
    pub fn reject_memory_item(
        &self,
        actor_id: &str,
        memory_id: i64,
    ) -> Result<MemoryItem, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        connection.execute(
            "UPDATE memory_items SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![MEMORY_STATUS_ARCHIVED, now_python(), memory_id],
        )?;
        drop(connection);
        self.get_memory_item(actor_id, memory_id)
    }

    pub fn pin_memory_item(
        &self,
        actor_id: &str,
        memory_id: i64,
        pinned: bool,
    ) -> Result<MemoryItem, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        connection.execute(
            "UPDATE memory_items SET pinned = ?1, updated_at = ?2 WHERE id = ?3",
            params![i64::from(pinned), now_python(), memory_id],
        )?;
        drop(connection);
        self.get_memory_item(actor_id, memory_id)
    }

    /// Pending-confirmation memories, newest first (mirrors `service.list_pending`).
    pub fn list_pending_items(
        &self,
        actor_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<MemoryItem>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM memory_items WHERE user_id = ?1 AND status = ?2 \
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            user_id,
            MEMORY_STATUS_PENDING_CONFIRMATION,
            bounded_limit(limit, 100, 500),
        ])?;
        collect_rows(rows, MemoryItem::from_row)
    }

    /// Dashboard aggregates (mirrors `api/memory.py::memory_stats`).
    pub fn memory_stats(&self, actor_id: &str) -> Result<MemoryStats, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let count = |status: &str| -> Result<i64, StoreError> {
            query_one(
                &connection,
                "SELECT COUNT(*) FROM memory_items WHERE user_id = ?1 AND status = ?2",
                params![user_id, status],
                |row| Ok(row.get(0)?),
            )?
            .ok_or(StoreError::NotFound("memory items"))
        };
        let grouped = |column: &str| -> Result<BTreeMap<String, i64>, StoreError> {
            let mut statement = connection.prepare(&format!(
                "SELECT {column}, COUNT(*) FROM memory_items \
                 WHERE user_id = ?1 AND status = ?2 GROUP BY {column}"
            ))?;
            let rows = statement.query(params![user_id, MEMORY_STATUS_ACTIVE])?;
            let pairs = collect_rows(rows, |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            Ok(pairs.into_iter().collect())
        };
        Ok(MemoryStats {
            total_active: count(MEMORY_STATUS_ACTIVE)?,
            by_type: grouped("memory_type")?,
            by_scope: grouped("scope")?,
            total_episodes: query_one(
                &connection,
                "SELECT COUNT(*) FROM episodes WHERE user_id = ?1",
                [user_id],
                |row| Ok(row.get(0)?),
            )?
            .ok_or(StoreError::NotFound("episodes"))?,
            total_archived: count(MEMORY_STATUS_ARCHIVED)?,
            total_pending: count(MEMORY_STATUS_PENDING_CONFIRMATION)?,
            total_entities: query_one(
                &connection,
                "SELECT COUNT(*) FROM entities WHERE user_id = ?1",
                [user_id],
                |row| Ok(row.get(0)?),
            )?
            .ok_or(StoreError::NotFound("entities"))?,
        })
    }
}

// --- Episodes ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: i64,
    pub user_id: i64,
    pub agent_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub run_id: Option<i64>,
    pub title: String,
    pub summary: String,
    pub outcome: String,
    pub importance: f64,
    pub tags: Option<Value>,
    pub related_entities: Option<Value>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Episode {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            agent_id: row.get("agent_id")?,
            conversation_id: row.get("conversation_id")?,
            run_id: row.get("run_id")?,
            title: row.get("title")?,
            summary: row.get("summary")?,
            outcome: row.get("outcome")?,
            importance: row.get("importance")?,
            tags: parse_json(row.get("tags")?)?,
            related_entities: parse_json(row.get("related_entities")?)?,
            started_at: row.get("started_at")?,
            ended_at: row.get("ended_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEpisode {
    pub agent_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub run_id: Option<i64>,
    pub title: String,
    pub summary: String,
    pub outcome: String,
    pub importance: f64,
    pub tags: Option<Value>,
    pub related_entities: Option<Value>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

impl Default for NewEpisode {
    fn default() -> Self {
        Self {
            agent_id: None,
            conversation_id: None,
            run_id: None,
            title: String::new(),
            summary: String::new(),
            outcome: "unknown".to_string(),
            importance: 0.5,
            tags: None,
            related_entities: None,
            started_at: None,
            ended_at: None,
        }
    }
}

fn fetch_episode(
    connection: &Connection,
    user_id: i64,
    episode_id: i64,
) -> Result<Option<Episode>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM episodes WHERE id = ?1 AND user_id = ?2",
        params![episode_id, user_id],
        Episode::from_row,
    )
}

impl crate::LegacyStore {
    pub fn list_episodes(
        &self,
        actor_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Episode>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM episodes WHERE user_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = statement.query(params![user_id, bounded_limit(limit, 20, 100)])?;
        collect_rows(rows, Episode::from_row)
    }

    pub fn get_episode(&self, actor_id: &str, episode_id: i64) -> Result<Episode, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_episode(&connection, user_id, episode_id)?.ok_or(StoreError::NotFound("episode"))
    }

    pub fn create_episode(&self, actor_id: &str, new: &NewEpisode) -> Result<Episode, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(conversation_id) = new.conversation_id {
            require_conversation(&connection, actor_id, conversation_id)?;
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO episodes(created_at, updated_at, user_id, agent_id, conversation_id,
               run_id, title, summary, outcome, importance, tags, related_entities, started_at,
               ended_at)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                timestamp,
                user_id,
                new.agent_id,
                new.conversation_id,
                new.run_id,
                new.title,
                new.summary,
                new.outcome,
                new.importance,
                json_text(&new.tags)?,
                json_text(&new.related_entities)?,
                new.started_at,
                new.ended_at,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_episode(actor_id, id)
    }
}

// --- Working memory ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingMemory {
    pub id: i64,
    pub conversation_id: i64,
    pub state: Value,
    pub summary: Option<String>,
    pub summary_up_to_message_id: Option<i64>,
    pub token_estimate: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkingMemory {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            state: parse_json(row.get("state")?)?.unwrap_or_else(|| json!({})),
            summary: row.get("summary")?,
            summary_up_to_message_id: row.get("summary_up_to_message_id")?,
            token_estimate: row.get("token_estimate")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl crate::LegacyStore {
    pub fn get_working_memory(
        &self,
        actor_id: &str,
        conversation_id: i64,
    ) -> Result<Option<WorkingMemory>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        query_one(
            &connection,
            "SELECT * FROM working_memory WHERE conversation_id = ?1",
            [conversation_id],
            WorkingMemory::from_row,
        )
    }

    /// Insert or update the per-conversation scratchpad/summary row.
    pub fn upsert_working_memory(
        &self,
        actor_id: &str,
        conversation_id: i64,
        state: &Value,
        summary: Option<&str>,
        summary_up_to_message_id: Option<i64>,
        token_estimate: Option<i64>,
    ) -> Result<WorkingMemory, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let state = if state.is_null() {
            json!({})
        } else {
            state.clone()
        };
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO working_memory(created_at, updated_at, conversation_id, state, summary,
               summary_up_to_message_id, token_estimate)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(conversation_id) DO UPDATE SET
               state = excluded.state,
               summary = excluded.summary,
               summary_up_to_message_id = excluded.summary_up_to_message_id,
               token_estimate = excluded.token_estimate,
               updated_at = excluded.updated_at",
            params![
                timestamp,
                conversation_id,
                json_text(&Some(state))?,
                summary,
                summary_up_to_message_id,
                token_estimate,
            ],
        )?;
        query_one(
            &connection,
            "SELECT * FROM working_memory WHERE conversation_id = ?1",
            [conversation_id],
            WorkingMemory::from_row,
        )?
        .ok_or(StoreError::NotFound("working memory"))
    }
}

// --- Entities ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub entity_type: String,
    pub aliases: Option<Value>,
    pub attributes: Option<Value>,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Entity {
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            name: row.get("name")?,
            entity_type: row.get("entity_type")?,
            aliases: parse_json(row.get("aliases")?)?,
            attributes: parse_json(row.get("attributes")?)?,
            description: row.get("description")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEntity {
    pub name: String,
    pub entity_type: String,
    pub aliases: Option<Value>,
    pub attributes: Option<Value>,
    pub description: Option<String>,
}

impl Default for NewEntity {
    fn default() -> Self {
        Self {
            name: String::new(),
            entity_type: "concept".to_string(),
            aliases: None,
            attributes: None,
            description: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityPatch {
    pub name: Option<String>,
    pub entity_type: Option<String>,
    pub aliases: Option<Value>,
    pub attributes: Option<Value>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRelation {
    pub id: i64,
    pub user_id: i64,
    pub source_entity_id: i64,
    pub target_entity_id: i64,
    pub relation_type: String,
    pub attributes: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl EntityRelation {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            source_entity_id: row.get("source_entity_id")?,
            target_entity_id: row.get("target_entity_id")?,
            relation_type: row.get("relation_type")?,
            attributes: parse_json(row.get("attributes")?)?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRelation {
    pub source_entity_id: i64,
    pub target_entity_id: i64,
    pub relation_type: String,
    pub attributes: Option<Value>,
}

impl Default for NewRelation {
    fn default() -> Self {
        Self {
            source_entity_id: 0,
            target_entity_id: 0,
            relation_type: "related_to".to_string(),
            attributes: None,
        }
    }
}

fn fetch_entity(
    connection: &Connection,
    user_id: i64,
    entity_id: i64,
) -> Result<Option<Entity>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM entities WHERE id = ?1 AND user_id = ?2",
        params![entity_id, user_id],
        Entity::from_row,
    )
}

fn require_entity(
    connection: &Connection,
    user_id: i64,
    entity_id: i64,
) -> Result<Entity, StoreError> {
    fetch_entity(connection, user_id, entity_id)?.ok_or(StoreError::NotFound("entity"))
}

fn request_relation(
    connection: &Connection,
    user_id: i64,
    relation_id: i64,
) -> Result<Option<EntityRelation>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM entity_relations WHERE id = ?1 AND user_id = ?2",
        params![relation_id, user_id],
        EntityRelation::from_row,
    )
}

fn entity_matches(entity: &Entity, needle_lower: &str) -> bool {
    if entity.name.to_lowercase().contains(needle_lower) {
        return true;
    }
    match &entity.aliases {
        Some(Value::Array(aliases)) => aliases
            .iter()
            .filter_map(Value::as_str)
            .any(|alias| alias.to_lowercase().contains(needle_lower)),
        _ => false,
    }
}

impl crate::LegacyStore {
    /// List entities, optionally filtered by type and a case-insensitive
    /// name/alias substring (mirrors `entities.list_entities`).
    pub fn list_entities(
        &self,
        actor_id: &str,
        search: Option<&str>,
        entity_type: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<Entity>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let limit = bounded_limit(limit, 100, 500) as usize;
        // Over-fetch for alias-only matches when a query is present.
        let fetch_limit = if search.is_some() { limit * 5 } else { limit } as i64;
        let sql = if entity_type.is_some() {
            "SELECT * FROM entities WHERE user_id = ?1 AND entity_type = ?2 \
             ORDER BY updated_at DESC LIMIT ?3"
        } else {
            "SELECT * FROM entities WHERE user_id = ?1 \
             ORDER BY updated_at DESC LIMIT ?2"
        };
        let mut statement = connection.prepare(sql)?;
        let rows = if let Some(entity_type) = entity_type {
            statement.query(params![user_id, entity_type, fetch_limit])?
        } else {
            statement.query(params![user_id, fetch_limit])?
        };
        let entities = collect_rows(rows, Entity::from_row)?;
        let Some(query) = search else {
            return Ok(entities);
        };
        let needle = query.to_lowercase();
        Ok(entities
            .into_iter()
            .filter(|entity| entity_matches(entity, &needle))
            .take(limit)
            .collect())
    }

    pub fn get_entity(&self, actor_id: &str, entity_id: i64) -> Result<Entity, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_entity(&connection, user_id, entity_id)
    }

    /// Create an entity; `(user_id, name)` is unique, so an existing canonical
    /// name yields [`StoreError::Conflict`].
    pub fn create_entity(&self, actor_id: &str, new: &NewEntity) -> Result<Entity, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let existing: Option<i64> = query_one(
            &connection,
            "SELECT id FROM entities WHERE user_id = ?1 AND name = ?2",
            params![user_id, new.name],
            |row| Ok(row.get(0)?),
        )?;
        if existing.is_some() {
            return Err(StoreError::Conflict(format!(
                "entity already exists: {}",
                new.name
            )));
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO entities(created_at, updated_at, user_id, name, entity_type, aliases,
               attributes, description)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                timestamp,
                user_id,
                new.name,
                new.entity_type,
                json_text(&new.aliases)?,
                json_text(&new.attributes)?,
                new.description,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_entity(actor_id, id)
    }

    pub fn update_entity(
        &self,
        actor_id: &str,
        entity_id: i64,
        patch: &EntityPatch,
    ) -> Result<Entity, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_entity(&connection, user_id, entity_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(name) = &patch.name {
            assignments.push("name = ?");
            values.push(Box::new(name.clone()));
        }
        if let Some(entity_type) = &patch.entity_type {
            assignments.push("entity_type = ?");
            values.push(Box::new(entity_type.clone()));
        }
        if patch.aliases.is_some() {
            assignments.push("aliases = ?");
            values.push(Box::new(json_text(&patch.aliases)?));
        }
        if patch.attributes.is_some() {
            assignments.push("attributes = ?");
            values.push(Box::new(json_text(&patch.attributes)?));
        }
        if patch.description.is_some() {
            assignments.push("description = ?");
            values.push(Box::new(patch.description.clone()));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(entity_id));
            let sql = format!(
                "UPDATE entities SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_entity(actor_id, entity_id)
    }

    /// Delete an entity plus its memory links and any relations touching it.
    pub fn delete_entity(&self, actor_id: &str, entity_id: i64) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_entity(&connection, user_id, entity_id)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM memory_item_entities WHERE entity_id = ?1",
            [entity_id],
        )?;
        transaction.execute(
            "DELETE FROM entity_relations WHERE source_entity_id = ?1 OR target_entity_id = ?1",
            [entity_id],
        )?;
        transaction.execute(
            "DELETE FROM entities WHERE id = ?1 AND user_id = ?2",
            params![entity_id, user_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Link a memory to an entity (idempotent). Returns `true` when a new link
    /// was inserted. Both rows must belong to the actor.
    pub fn link_memory_entity(
        &self,
        actor_id: &str,
        memory_id: i64,
        entity_id: i64,
    ) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        require_entity(&connection, user_id, entity_id)?;
        let changed = connection.execute(
            "INSERT OR IGNORE INTO memory_item_entities(memory_id, entity_id) VALUES (?1, ?2)",
            params![memory_id, entity_id],
        )?;
        Ok(changed > 0)
    }

    pub fn unlink_memory_entity(
        &self,
        actor_id: &str,
        memory_id: i64,
        entity_id: i64,
    ) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        let changed = connection.execute(
            "DELETE FROM memory_item_entities WHERE memory_id = ?1 AND entity_id = ?2",
            params![memory_id, entity_id],
        )?;
        Ok(changed > 0)
    }

    pub fn list_memory_entities(
        &self,
        actor_id: &str,
        memory_id: i64,
    ) -> Result<Vec<Entity>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        let mut statement = connection.prepare(
            "SELECT e.* FROM entities e \
             JOIN memory_item_entities l ON l.entity_id = e.id \
             WHERE l.memory_id = ?1 AND e.user_id = ?2 ORDER BY e.name",
        )?;
        let rows = statement.query(params![memory_id, user_id])?;
        collect_rows(rows, Entity::from_row)
    }

    /// Active memories linked to an entity, newest first (mirrors
    /// `entities.memories_for_entity(active_only=True)`).
    pub fn list_entity_memories(
        &self,
        actor_id: &str,
        entity_id: i64,
        limit: Option<usize>,
    ) -> Result<Vec<MemoryItem>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_entity(&connection, user_id, entity_id)?;
        let mut statement = connection.prepare(
            "SELECT m.* FROM memory_items m \
             JOIN memory_item_entities l ON l.memory_id = m.id \
             WHERE l.entity_id = ?1 AND m.user_id = ?2 AND m.status = ?3 \
             ORDER BY m.updated_at DESC LIMIT ?4",
        )?;
        let rows = statement.query(params![
            entity_id,
            user_id,
            MEMORY_STATUS_ACTIVE,
            bounded_limit(limit, 100, 500),
        ])?;
        collect_rows(rows, MemoryItem::from_row)
    }

    /// Create a directed relation (idempotent on user + pair + type).
    pub fn create_relation(
        &self,
        actor_id: &str,
        new: &NewRelation,
    ) -> Result<EntityRelation, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_entity(&connection, user_id, new.source_entity_id)?;
        require_entity(&connection, user_id, new.target_entity_id)?;
        let relation_type = if new.relation_type.is_empty() {
            "related_to"
        } else {
            new.relation_type.as_str()
        };
        if let Some(existing) = query_one(
            &connection,
            "SELECT * FROM entity_relations WHERE user_id = ?1 AND source_entity_id = ?2 \
             AND target_entity_id = ?3 AND relation_type = ?4",
            params![
                user_id,
                new.source_entity_id,
                new.target_entity_id,
                relation_type
            ],
            EntityRelation::from_row,
        )? {
            return Ok(existing);
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO entity_relations(created_at, updated_at, user_id, source_entity_id,
               target_entity_id, relation_type, attributes)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                timestamp,
                user_id,
                new.source_entity_id,
                new.target_entity_id,
                relation_type,
                json_text(&new.attributes)?,
            ],
        )?;
        let id = connection.last_insert_rowid();
        request_relation(&connection, user_id, id)?.ok_or(StoreError::NotFound("entity relation"))
    }

    pub fn delete_relation(&self, actor_id: &str, relation_id: i64) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let changed = connection.execute(
            "DELETE FROM entity_relations WHERE id = ?1 AND user_id = ?2",
            params![relation_id, user_id],
        )?;
        Ok(changed > 0)
    }

    pub fn list_relations(
        &self,
        actor_id: &str,
        entity_id: i64,
    ) -> Result<Vec<EntityRelation>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_entity(&connection, user_id, entity_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM entity_relations WHERE user_id = ?1 \
             AND (source_entity_id = ?2 OR target_entity_id = ?2) ORDER BY id",
        )?;
        let rows = statement.query(params![user_id, entity_id])?;
        collect_rows(rows, EntityRelation::from_row)
    }
}

// --- Embedding bookkeeping ---

impl crate::LegacyStore {
    /// Upsert the embedding metadata row for a memory (the vector itself lives
    /// in the optional sqlite-vec `memory_vec` table, which this schema
    /// deliberately omits).
    pub fn record_memory_embedding(
        &self,
        actor_id: &str,
        memory_id: i64,
        model: &str,
        dimension: i64,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO memory_embeddings(memory_id, model, dimension, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(memory_id) DO UPDATE SET
               model = excluded.model,
               dimension = excluded.dimension,
               updated_at = excluded.updated_at",
            params![memory_id, model, dimension, timestamp],
        )?;
        Ok(())
    }

    pub fn delete_memory_embedding(
        &self,
        actor_id: &str,
        memory_id: i64,
    ) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        require_memory_item(&connection, user_id, memory_id)?;
        let changed = connection.execute(
            "DELETE FROM memory_embeddings WHERE memory_id = ?1",
            [memory_id],
        )?;
        Ok(changed > 0)
    }

    /// Active memories that have no embedding row yet (backfill targets,
    /// mirrors `embeddings.missing_embedding_memory_ids`).
    pub fn list_memories_missing_embeddings(
        &self,
        actor_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<i64>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT id FROM memory_items \
             WHERE user_id = ?1 AND status = ?2 \
             AND id NOT IN (SELECT memory_id FROM memory_embeddings) \
             ORDER BY id LIMIT ?3",
        )?;
        let rows = statement.query(params![
            user_id,
            MEMORY_STATUS_ACTIVE,
            bounded_limit(limit, 100, 1_000),
        ])?;
        collect_rows(rows, |row| Ok(row.get(0)?))
    }
}
