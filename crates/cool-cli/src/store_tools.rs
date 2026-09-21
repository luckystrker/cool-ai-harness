//! Store-backed agent tools (M11 WS2).
//!
//! The Rust agent runtime ships six builtins; this module adds the first
//! store-backed parity slice for the memory family. Handlers read the
//! server-derived actor from [`ToolContext`] (never from arguments), execute
//! through `cool-store`, and mask their output before it reaches the event log.
//! They are registered only when the CLI serves a legacy store, so a store-less
//! run keeps the original six-tool registry.

use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{Tool, ToolContext, ToolDefinition, ToolError, ToolHandler, ToolResult};
use cool_security::Decision;
use cool_store::LegacyStore;
use cool_store::domains::memory::{
    MAX_AGENT_IMPORTANCE, MEMORY_STATUS_ACTIVE, MemoryFilter, MemoryItemPatch, NewMemoryItem,
    SCOPE_CONVERSATION,
};
use serde_json::{Value, json};

/// Registers the store-backed memory tools over one legacy store.
pub fn store_tool_registry(store: Arc<LegacyStore>) -> Result<Vec<Tool>, ToolError> {
    Ok(vec![
        Tool::new(
            definition(
                "memory_remember",
                "Store a new long-term memory. Use this to remember important facts, user preferences, procedures, or events that should persist across sessions.",
                json!({"type":"object","properties":{"content":{"type":"string"},"memory_type":{"type":"string"},"importance":{"type":"number","minimum":0.0,"maximum":1.0},"tags":{"type":"array","items":{"type":"string"}},"scope":{"type":"string"}},"required":["content"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            MemoryRemember {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "memory_recall",
                "Search long-term memories by query using a lexical content filter (not semantic ranking), ordered by importance.",
                json!({"type":"object","properties":{"query":{"type":"string"},"memory_type":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            MemoryRecall {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "memory_forget",
                "Archive or permanently delete a memory by its ID.",
                json!({"type":"object","properties":{"memory_id":{"type":"integer"},"hard":{"type":"boolean"}},"required":["memory_id"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            MemoryForget {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "memory_update",
                "Update the content, importance, or tags of an existing memory.",
                json!({"type":"object","properties":{"memory_id":{"type":"integer"},"content":{"type":"string"},"importance":{"type":"number","minimum":0.0,"maximum":1.0},"tags":{"type":"array","items":{"type":"string"}}},"required":["memory_id"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            MemoryUpdate {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "memory_list",
                "List recent memories, optionally filtered by type.",
                json!({"type":"object","properties":{"memory_type":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":50}},"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            MemoryList {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "set_working_memory",
                "Set a key-value pair in the working memory scratchpad for the current conversation. Useful for tracking goals, hypotheses, and entity states.",
                json!({"type":"object","properties":{"key":{"type":"string"},"value":{"type":"string"}},"required":["key","value"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            SetWorkingMemory {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "get_working_memory",
                "Read from the working memory scratchpad. Pass a key to get a specific value, or omit key to get the entire state.",
                json!({"type":"object","properties":{"key":{"type":"string"}},"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            GetWorkingMemory {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "entity_lookup",
                "Look up a named entity (person, project, service, tool, concept) by name or alias and return its structured records.",
                json!({"type":"object","properties":{"query":{"type":"string"},"entity_type":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            EntityLookup { store },
        ),
    ])
}

fn definition(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
    }
}

struct MemoryRemember {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for MemoryRemember {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &["content", "memory_type", "importance", "tags", "scope"],
        )?;
        let content = required_string(&arguments, "content")?;
        let memory_type =
            optional_string(&arguments, "memory_type")?.unwrap_or_else(|| "semantic".to_owned());
        let importance = optional_f64(&arguments, "importance")?.unwrap_or(0.5);
        let tags = optional_string_array(&arguments, "tags")?;
        let scope =
            optional_string(&arguments, "scope")?.unwrap_or_else(|| SCOPE_CONVERSATION.to_owned());
        let new = NewMemoryItem {
            scope,
            memory_type,
            content: content.to_owned(),
            tags: tags.map(Value::from),
            importance,
            source: "agent".to_owned(),
            // Bind a conversation-scoped write to the run's conversation so the
            // store can attach the project key (visibility across the project).
            conversation_id: context.conversation_id,
            ..NewMemoryItem::default()
        };
        match self.store.create_memory_item(&context.actor_id, &new) {
            Ok(item) => Ok(ToolResult::ok(json!({
                "id": item.id,
                "status": item.status,
                "memory_type": item.memory_type,
                "scope": item.scope,
                "importance": item.importance,
                "content": item.content,
            }))
            .masked()),
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

struct MemoryRecall {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for MemoryRecall {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["query", "memory_type", "limit"])?;
        let query = required_string(&arguments, "query")?.to_lowercase();
        let memory_type = optional_string(&arguments, "memory_type")?;
        let limit = bounded(optional_i64(&arguments, "limit")?, 5, 1, 20) as usize;
        let filter = MemoryFilter {
            memory_type,
            status: Some(MEMORY_STATUS_ACTIVE.to_owned()),
            // Over-fetch before the lexical filter, capped by the store.
            limit: Some(limit.saturating_mul(5).min(100)),
            ..MemoryFilter::default()
        };
        match self.store.list_memory_items(&context.actor_id, &filter) {
            Ok(items) => {
                let matches = items
                    .into_iter()
                    .filter(|item| item.content.to_lowercase().contains(&query))
                    .take(limit)
                    .map(|item| serde_json::to_value(&item).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                Ok(ToolResult::ok(Value::Array(matches)).masked())
            }
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

struct MemoryForget {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for MemoryForget {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["memory_id", "hard"])?;
        let memory_id = required_i64(&arguments, "memory_id")?;
        let hard = optional_bool(&arguments, "hard")?.unwrap_or(false);
        match self
            .store
            .delete_memory_item(&context.actor_id, memory_id, hard)
        {
            Ok(()) => Ok(ToolResult::ok(json!({"memory_id": memory_id, "hard": hard})).masked()),
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

struct MemoryUpdate {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for MemoryUpdate {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["memory_id", "content", "importance", "tags"])?;
        let memory_id = required_i64(&arguments, "memory_id")?;
        let patch = MemoryItemPatch {
            content: optional_string(&arguments, "content")?,
            // An agent-sourced update must not exceed the agent importance cap
            // (Python passes cap_agent_importance for this tool path).
            importance: optional_f64(&arguments, "importance")?
                .map(|value| value.clamp(0.0, MAX_AGENT_IMPORTANCE)),
            tags: optional_string_array(&arguments, "tags")?.map(Value::from),
            ..MemoryItemPatch::default()
        };
        match self
            .store
            .update_memory_item(&context.actor_id, memory_id, &patch)
        {
            Ok(item) => {
                Ok(ToolResult::ok(serde_json::to_value(&item).unwrap_or(Value::Null)).masked())
            }
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

struct MemoryList {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for MemoryList {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["memory_type", "limit"])?;
        let filter = MemoryFilter {
            memory_type: optional_string(&arguments, "memory_type")?,
            status: Some(MEMORY_STATUS_ACTIVE.to_owned()),
            limit: Some(bounded(optional_i64(&arguments, "limit")?, 10, 1, 50) as usize),
            ..MemoryFilter::default()
        };
        match self.store.list_memory_items(&context.actor_id, &filter) {
            Ok(items) => {
                let payload = items
                    .into_iter()
                    .map(|item| serde_json::to_value(&item).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                Ok(ToolResult::ok(Value::Array(payload)).masked())
            }
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

struct SetWorkingMemory {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for SetWorkingMemory {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["key", "value"])?;
        let key = required_string(&arguments, "key")?.to_owned();
        let value = required_string(&arguments, "value")?;
        let Some(conversation_id) = context.conversation_id else {
            return Ok(ToolResult::error(
                "no_active_conversation",
                "No active conversation for working memory",
            ));
        };
        // Read-modify-write so the scratchpad merges (Python
        // `update_working_memory_state`) instead of replacing the whole state.
        let existing = match self
            .store
            .get_working_memory(&context.actor_id, conversation_id)
        {
            Ok(existing) => existing,
            Err(error) => {
                return Ok(ToolResult::error("memory_store_failed", error.to_string()).masked());
            }
        };
        let mut state = existing
            .as_ref()
            .map(|row| row.state.clone())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        state
            .as_object_mut()
            .expect("state is an object")
            .insert(key.clone(), parse_working_value(value));
        let result = self.store.upsert_working_memory(
            &context.actor_id,
            conversation_id,
            &state,
            existing.as_ref().and_then(|row| row.summary.as_deref()),
            existing
                .as_ref()
                .and_then(|row| row.summary_up_to_message_id),
            existing.as_ref().and_then(|row| row.token_estimate),
        );
        match result {
            Ok(_) => Ok(ToolResult::ok(json!({"key": key, "status": "set"})).masked()),
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

struct GetWorkingMemory {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for GetWorkingMemory {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["key"])?;
        let key = optional_string(&arguments, "key")?;
        let Some(conversation_id) = context.conversation_id else {
            return Ok(ToolResult::error(
                "no_active_conversation",
                "No active conversation for working memory",
            ));
        };
        let existing = match self
            .store
            .get_working_memory(&context.actor_id, conversation_id)
        {
            Ok(existing) => existing,
            Err(error) => {
                return Ok(ToolResult::error("memory_store_failed", error.to_string()).masked());
            }
        };
        match key {
            Some(key) => {
                let value = existing
                    .and_then(|row| row.state.get(&key).cloned())
                    .unwrap_or(Value::Null);
                Ok(ToolResult::ok(json!({"key": key, "value": value})).masked())
            }
            None => Ok(
                ToolResult::ok(existing.map(|row| row.state).unwrap_or_else(|| json!({}))).masked(),
            ),
        }
    }
}

struct EntityLookup {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for EntityLookup {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["query", "entity_type", "limit"])?;
        let query = required_string(&arguments, "query")?;
        let entity_type = optional_string(&arguments, "entity_type")?;
        let limit = bounded(optional_i64(&arguments, "limit")?, 5, 1, 20) as usize;
        match self.store.list_entities(
            &context.actor_id,
            Some(query),
            entity_type.as_deref(),
            Some(limit),
        ) {
            Ok(entities) => {
                let payload = entities
                    .into_iter()
                    .map(|entity| serde_json::to_value(&entity).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                Ok(ToolResult::ok(Value::Array(payload)).masked())
            }
            Err(error) => Ok(ToolResult::error("memory_store_failed", error.to_string()).masked()),
        }
    }
}

fn reject_unknown(arguments: &Value, allowed: &[&str]) -> Result<(), ToolError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("arguments must be an object".to_owned()))?;
    if let Some(name) = object.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(ToolError::InvalidArguments(format!(
            "unknown argument {name}"
        )));
    }
    Ok(())
}

fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be a non-empty string")))
}

fn optional_string(arguments: &Value, name: &str) -> Result<Option<String>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "{name} must be a non-empty string"
        ))),
    }
}

fn optional_f64(arguments: &Value, name: &str) -> Result<Option<f64>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be a number"))),
    }
}

fn optional_i64(arguments: &Value, name: &str) -> Result<Option<i64>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be an integer"))),
    }
}

fn required_i64(arguments: &Value, name: &str) -> Result<i64, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be an integer")))
}

fn optional_bool(arguments: &Value, name: &str) -> Result<Option<bool>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be a boolean"))),
    }
}

fn optional_string_array(arguments: &Value, name: &str) -> Result<Option<Vec<String>>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    ToolError::InvalidArguments(format!("{name} must contain strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "{name} must be an array"
        ))),
    }
}

fn bounded(value: Option<i64>, default: i64, min: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(min, max)
}

/// Working-memory values are strings that may carry JSON (Python tries
/// `json.loads` and falls back to the raw string).
fn parse_working_value(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()))
}
