//! Conversations and messages (legacy `conversations` / `messages` tables).

use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, require_conversation,
    user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: i64,
    pub user_id: i64,
    pub title: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub working_directory: Option<String>,
    pub permissions: Option<Value>,
    pub capability_policy: Option<Value>,
    pub profile_id: Option<i64>,
    pub tags: Option<Value>,
    pub folder: Option<String>,
    pub is_pinned: bool,
    pub is_archived: bool,
    pub metadata: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl Conversation {
    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            title: row.get("title")?,
            provider: row.get("provider")?,
            model: row.get("model")?,
            working_directory: row.get("working_directory")?,
            permissions: parse_json(row.get("permissions")?)?,
            capability_policy: parse_json(row.get("capability_policy")?)?,
            profile_id: row.get("profile_id")?,
            tags: parse_json(row.get("tags")?)?,
            folder: row.get("folder")?,
            is_pinned: row.get::<_, i64>("is_pinned")? != 0,
            is_archived: row.get::<_, i64>("is_archived")? != 0,
            metadata: parse_json(row.get("metadata_")?)?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConversationFilter {
    /// Hide machine-owned conversations unless explicitly requested.
    pub include_machine_owned: bool,
    /// `Some(true)` only archived, `Some(false)` only active.
    pub archived: Option<bool>,
    pub pinned: Option<bool>,
    pub folder: Option<String>,
    pub search: Option<String>,
    pub limit: Option<usize>,
    pub offset: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewConversation {
    pub title: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub working_directory: Option<String>,
    pub permissions: Option<Value>,
    pub capability_policy: Option<Value>,
    pub profile_id: Option<i64>,
    pub tags: Option<Value>,
    pub folder: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPatch {
    pub title: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Empty string clears the working directory (Python sentinel convention).
    pub working_directory: Option<String>,
    pub permissions: Option<Value>,
    pub capability_policy: Option<Value>,
    /// `-1` clears the profile (Python sentinel convention).
    pub profile_id: Option<i64>,
    pub tags: Option<Value>,
    pub folder: Option<String>,
    pub is_pinned: Option<bool>,
    pub is_archived: Option<bool>,
    pub metadata: Option<Value>,
}

impl crate::LegacyStore {
    pub fn list_conversations(
        &self,
        actor_id: &str,
        filter: &ConversationFilter,
    ) -> Result<Vec<Conversation>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut sql = String::from(
            "SELECT * FROM conversations WHERE user_id = ?1 \
             AND (?2 IS NULL OR is_archived = ?2) \
             AND (?3 IS NULL OR is_pinned = ?3) \
             AND (?4 IS NULL OR folder = ?4) \
             AND (?5 IS NULL OR title LIKE ?5) \
             AND (?6 = 1 OR COALESCE(json_extract(metadata_, '$.is_subagent'), 0) = 0) \
             AND (?6 = 1 OR COALESCE(json_extract(metadata_, '$.is_task'), 0) = 0) \
             ORDER BY updated_at DESC, id DESC LIMIT ?7 OFFSET ?8",
        );
        if filter.search.is_some() {
            sql = sql.replace(
                "AND (?5 IS NULL OR title LIKE ?5)",
                "AND (?5 IS NULL OR title LIKE ?5 OR EXISTS (SELECT 1 FROM messages m \
                 WHERE m.conversation_id = conversations.id AND m.content LIKE ?5))",
            );
        }
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query(params![
            user_id,
            filter.archived.map(i64::from),
            filter.pinned.map(i64::from),
            filter.folder,
            filter.search.as_ref().map(|value| format!("%{value}%")),
            i64::from(filter.include_machine_owned),
            bounded_limit(filter.limit, 100, 500),
            filter.offset as i64,
        ])?;
        collect_rows(rows, Conversation::from_row)
    }

    pub fn get_conversation(
        &self,
        actor_id: &str,
        conversation_id: i64,
    ) -> Result<Conversation, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        query_one(
            &connection,
            "SELECT * FROM conversations WHERE id = ?1",
            [conversation_id],
            Conversation::from_row,
        )?
        .ok_or(StoreError::NotFound("conversation"))
    }

    pub fn create_conversation(
        &self,
        actor_id: &str,
        new: &NewConversation,
    ) -> Result<Conversation, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO conversations(created_at, updated_at, user_id, title, provider, model,
               working_directory, permissions, capability_policy, profile_id, tags, folder,
               is_pinned, is_archived, metadata_)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0, ?12)",
            params![
                timestamp,
                user_id,
                new.title,
                new.provider,
                new.model,
                new.working_directory,
                json_text(&new.permissions)?,
                json_text(&new.capability_policy)?,
                new.profile_id,
                json_text(&new.tags)?,
                new.folder,
                json_text(&new.metadata)?,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_conversation(actor_id, id)
    }

    pub fn update_conversation(
        &self,
        actor_id: &str,
        conversation_id: i64,
        patch: &ConversationPatch,
    ) -> Result<Conversation, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(title) = &patch.title {
            assignments.push("title = ?");
            values.push(Box::new(title.clone()));
        }
        if let Some(provider) = &patch.provider {
            assignments.push("provider = ?");
            values.push(Box::new(provider.clone()));
        }
        if let Some(model) = &patch.model {
            assignments.push("model = ?");
            values.push(Box::new(model.clone()));
        }
        if let Some(directory) = &patch.working_directory {
            assignments.push("working_directory = ?");
            values.push(Box::new(if directory.is_empty() {
                None
            } else {
                Some(directory.clone())
            }));
        }
        if patch.permissions.is_some() {
            assignments.push("permissions = ?");
            values.push(Box::new(json_text(&patch.permissions)?));
        }
        if patch.capability_policy.is_some() {
            assignments.push("capability_policy = ?");
            values.push(Box::new(json_text(&patch.capability_policy)?));
        }
        if let Some(profile_id) = patch.profile_id {
            assignments.push("profile_id = ?");
            values.push(Box::new(if profile_id == -1 {
                None
            } else {
                Some(profile_id)
            }));
        }
        if patch.tags.is_some() {
            assignments.push("tags = ?");
            values.push(Box::new(json_text(&patch.tags)?));
        }
        if let Some(folder) = &patch.folder {
            assignments.push("folder = ?");
            values.push(Box::new(if folder.is_empty() {
                None
            } else {
                Some(folder.clone())
            }));
        }
        if let Some(pinned) = patch.is_pinned {
            assignments.push("is_pinned = ?");
            values.push(Box::new(i64::from(pinned)));
        }
        if let Some(archived) = patch.is_archived {
            assignments.push("is_archived = ?");
            values.push(Box::new(i64::from(archived)));
        }
        if patch.metadata.is_some() {
            assignments.push("metadata_ = ?");
            values.push(Box::new(json_text(&patch.metadata)?));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(conversation_id));
            let sql = format!(
                "UPDATE conversations SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_conversation(actor_id, conversation_id)
    }

    /// Delete a conversation and its messages, matching the Python service
    /// (which removes only those two tables and leaves historical run rows).
    pub fn delete_conversation(
        &self,
        actor_id: &str,
        conversation_id: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM messages WHERE conversation_id = ?1",
            [conversation_id],
        )?;
        transaction.execute("DELETE FROM conversations WHERE id = ?1", [conversation_id])?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Value>,
    pub tool_result: Option<Value>,
    pub usage: Option<Value>,
    pub thinking: Option<String>,
    pub model: Option<String>,
    pub duration_ms: Option<i64>,
    pub artifact_ids: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl Message {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            role: row.get("role")?,
            content: row.get("content")?,
            tool_calls: parse_json(row.get("tool_calls")?)?,
            tool_result: parse_json(row.get("tool_result")?)?,
            usage: parse_json(row.get("usage")?)?,
            thinking: row.get("thinking")?,
            model: row.get("model")?,
            duration_ms: row.get("duration_ms")?,
            artifact_ids: parse_json(row.get("artifact_ids")?)?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMessage {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Value>,
    pub tool_result: Option<Value>,
    pub usage: Option<Value>,
    pub thinking: Option<String>,
    pub model: Option<String>,
    pub duration_ms: Option<i64>,
    pub artifact_ids: Option<Value>,
}

#[derive(Clone, Debug, Default)]
pub struct MessagePage {
    /// Return messages with `id` strictly lower than this cursor.
    pub before_id: Option<i64>,
    /// Return messages with `id` strictly greater than this cursor.
    pub after_id: Option<i64>,
    pub limit: Option<usize>,
}

/// Fetch a message without re-locking the store.
fn fetch_message(
    connection: &rusqlite::Connection,
    conversation_id: i64,
    message_id: i64,
) -> Result<Message, StoreError> {
    query_one(
        connection,
        "SELECT * FROM messages WHERE id = ?1 AND conversation_id = ?2",
        params![message_id, conversation_id],
        Message::from_row,
    )?
    .ok_or(StoreError::NotFound("message"))
}

impl crate::LegacyStore {
    /// Chronological message page (oldest first), optionally bounded by id.
    pub fn list_messages(
        &self,
        actor_id: &str,
        conversation_id: i64,
        page: &MessagePage,
    ) -> Result<Vec<Message>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM messages WHERE conversation_id = ?1 \
             AND (?2 IS NULL OR id < ?2) AND (?3 IS NULL OR id > ?3) \
             ORDER BY id LIMIT ?4",
        )?;
        let rows = statement.query(params![
            conversation_id,
            page.before_id,
            page.after_id,
            bounded_limit(page.limit, 500, 2_000),
        ])?;
        collect_rows(rows, Message::from_row)
    }

    pub fn add_message(
        &self,
        actor_id: &str,
        conversation_id: i64,
        new: &NewMessage,
    ) -> Result<Message, StoreError> {
        let mut connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO messages(created_at, updated_at, conversation_id, role, content,
               tool_calls, tool_result, usage, thinking, model, duration_ms, artifact_ids)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                timestamp,
                conversation_id,
                new.role,
                new.content,
                json_text(&new.tool_calls)?,
                json_text(&new.tool_result)?,
                json_text(&new.usage)?,
                new.thinking,
                new.model,
                new.duration_ms,
                json_text(&new.artifact_ids)?,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        drop(connection);
        self.get_message(actor_id, conversation_id, id)
    }

    /// Fetch a message, scoped to the conversation owner.
    pub fn get_message(
        &self,
        actor_id: &str,
        conversation_id: i64,
        message_id: i64,
    ) -> Result<Message, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        fetch_message(&connection, conversation_id, message_id)
    }

    /// Update the streaming/final fields of a message row.
    #[allow(clippy::too_many_arguments)]
    pub fn update_message_content(
        &self,
        actor_id: &str,
        conversation_id: i64,
        message_id: i64,
        content: Option<&str>,
        thinking: Option<&str>,
        usage: Option<&Value>,
        duration_ms: Option<i64>,
    ) -> Result<Message, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        connection.execute(
            "UPDATE messages SET content = COALESCE(?1, content), thinking = COALESCE(?2, thinking),
               usage = COALESCE(?3, usage), duration_ms = COALESCE(?4, duration_ms), updated_at = ?5
             WHERE id = ?6 AND conversation_id = ?7",
            params![
                content,
                thinking,
                usage.map(serde_json::to_string).transpose()?,
                duration_ms,
                now_python(),
                message_id,
                conversation_id,
            ],
        )?;
        fetch_message(&connection, conversation_id, message_id)
    }
}

/// Parse a message's `artifact_ids` list (Python stores a JSON array).
#[allow(dead_code)]
pub(crate) fn artifact_ids(message: &Message) -> Vec<i64> {
    message
        .artifact_ids
        .as_ref()
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}
