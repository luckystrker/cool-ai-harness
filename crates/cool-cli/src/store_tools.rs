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
use cool_security::{Capability, Decision};
use cool_store::domains::memory::{
    MAX_AGENT_IMPORTANCE, MEMORY_STATUS_ACTIVE, MemoryFilter, MemoryItemPatch, NewMemoryItem,
    SCOPE_CONVERSATION,
};
use cool_store::domains::rss::NewRssSubscription;
use cool_store::domains::tasks::{NewScheduledTask, ScheduledTask, ScheduledTaskPatch};
use cool_store::domains::wiki::{NewWikiArticle, WikiArticlePatch, WikiFilter};
use cool_store::scheduler::{describe_cron, parse_natural_schedule};
use cool_store::{LegacyStore, StoreError};
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
                "read_wiki",
                "Read a wiki article by id, or find one by title.",
                json!({"type":"object","properties":{"article_id":{"type":"integer"},"title":{"type":"string"}},"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            ReadWiki {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "write_wiki",
                "Create a new wiki article from Markdown content.",
                json!({"type":"object","properties":{"title":{"type":"string"},"content":{"type":"string"},"category":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}}},"required":["title","content"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            WriteWiki {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "search_wiki",
                "Search wiki articles by title and content substring, optionally filtered by category.",
                json!({"type":"object","properties":{"query":{"type":"string"},"category":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":50}},"required":["query"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            SearchWiki {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "update_wiki",
                "Update an existing wiki article's title, content, category, or tags.",
                json!({"type":"object","properties":{"article_id":{"type":"integer"},"title":{"type":"string"},"content":{"type":"string"},"category":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}}},"required":["article_id"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            UpdateWiki {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "rss_list",
                "List RSS/Atom subscriptions (optionally filtered by category).",
                json!({"type":"object","properties":{"category":{"type":"string"}},"additionalProperties":false}),
            ),
            [Capability::Read],
            Decision::Allow,
            RssList {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "rss_subscribe",
                "Subscribe to an RSS/Atom feed URL.",
                json!({"type":"object","properties":{"url":{"type":"string"},"category":{"type":"string"}},"required":["url"],"additionalProperties":false}),
            ),
            [Capability::Network],
            Decision::Allow,
            RssSubscribe {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "rss_unsubscribe",
                "Remove an RSS subscription and its stored entries.",
                json!({"type":"object","properties":{"subscription_id":{"type":"integer"}},"required":["subscription_id"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            RssUnsubscribe {
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
            EntityLookup {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "create_task",
                "Create a recurring (cron) task that runs a prompt on a schedule and delivers the result. Use when the user asks for something to happen regularly ('every Monday at 9am send me a digest'). The schedule may be a cron expression or a natural-language phrase.",
                json!({"type":"object","properties":{"name":{"type":"string"},"prompt":{"type":"string"},"schedule":{"type":"string"},"template":{"type":"string"},"timezone":{"type":"string"},"model":{"type":"string"},"tools":{"type":"array","items":{"type":"string"}},"delivery_channels":{"type":"array","items":{"type":"string"}},"quiet_hours_start":{"type":"string"},"quiet_hours_end":{"type":"string"},"enabled":{"type":"boolean"}},"required":["name"],"additionalProperties":false}),
            ),
            [Capability::Execute],
            Decision::Ask,
            CreateTask {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "list_tasks",
                "List the user's recurring tasks with their schedule, next run time and last status.",
                json!({"type":"object","properties":{"enabled_only":{"type":"boolean"}},"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            ListTasks {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "update_task",
                "Update a recurring task: rename it, change its prompt, schedule, model, tools or delivery channels, or pause/resume it.",
                json!({"type":"object","properties":{"task_id":{"type":"integer"},"name":{"type":"string"},"prompt":{"type":"string"},"schedule":{"type":"string"},"timezone":{"type":"string"},"model":{"type":"string"},"tools":{"type":"array","items":{"type":"string"}},"delivery_channels":{"type":"array","items":{"type":"string"}},"enabled":{"type":"boolean"}},"required":["task_id"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            UpdateTask {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "delete_task",
                "Delete a recurring task and its run history.",
                json!({"type":"object","properties":{"task_id":{"type":"integer"}},"required":["task_id"],"additionalProperties":false}),
            ),
            [],
            Decision::Ask,
            DeleteTask {
                store: store.clone(),
            },
        ),
        Tool::new(
            definition(
                "parse_cron",
                "Translate a natural-language schedule ('каждый день в 8 вечера', 'every weekday at 7:30') into a 5-field cron expression plus the next few run times.",
                json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            ParseCron,
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

struct ReadWiki {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for ReadWiki {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["article_id", "title"])?;
        let article_id = optional_i64(&arguments, "article_id")?;
        let title = optional_string(&arguments, "title")?;
        if article_id.is_none() && title.is_none() {
            return Err(ToolError::InvalidArguments(
                "article_id or title is required".to_owned(),
            ));
        }
        let article = if let Some(article_id) = article_id {
            match self.store.get_article(&context.actor_id, article_id) {
                Ok(article) => article,
                Err(StoreError::NotFound(_)) => {
                    return Ok(ToolResult::error("wiki_not_found", "article not found").masked());
                }
                Err(error) => {
                    return Ok(ToolResult::error("wiki_store_failed", error.to_string()).masked());
                }
            }
        } else {
            // Python matches the *title* only (case-insensitive contains) over the
            // recent non-archived articles; the store's `search` LIKE also covers
            // content/category/tags, so filter by title here.
            let query = title.expect("title is present").to_lowercase();
            let filter = WikiFilter {
                archived: Some(false),
                limit: Some(100),
                ..WikiFilter::default()
            };
            match self.store.list_articles(&context.actor_id, &filter) {
                Ok(articles) => match articles
                    .into_iter()
                    .find(|article| article.title.to_lowercase().contains(&query))
                {
                    Some(article) => article,
                    None => {
                        return Ok(ToolResult::error(
                            "wiki_not_found",
                            "no article matches that title",
                        )
                        .masked());
                    }
                },
                Err(error) => {
                    return Ok(ToolResult::error("wiki_store_failed", error.to_string()).masked());
                }
            }
        };
        Ok(ToolResult::ok(serde_json::to_value(&article).unwrap_or(Value::Null)).masked())
    }
}

struct WriteWiki {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for WriteWiki {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["title", "content", "category", "tags"])?;
        let new = NewWikiArticle {
            title: required_string(&arguments, "title")?.to_owned(),
            content: required_string(&arguments, "content")?.to_owned(),
            category: optional_string(&arguments, "category")?
                .unwrap_or_else(|| "general".to_owned()),
            tags: optional_string_array(&arguments, "tags")?.map(Value::from),
            source: "agent".to_owned(),
            source_memory_id: None,
            project_key: None,
            metadata: None,
        };
        match self.store.create_article(&context.actor_id, &new) {
            Ok(article) => {
                Ok(ToolResult::ok(serde_json::to_value(&article).unwrap_or(Value::Null)).masked())
            }
            Err(error) => Ok(ToolResult::error("wiki_store_failed", error.to_string()).masked()),
        }
    }
}

struct SearchWiki {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for SearchWiki {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["query", "category", "limit"])?;
        let filter = WikiFilter {
            search: Some(required_string(&arguments, "query")?.to_owned()),
            category: optional_string(&arguments, "category")?,
            archived: Some(false),
            limit: Some(bounded(optional_i64(&arguments, "limit")?, 10, 1, 50) as usize),
            ..WikiFilter::default()
        };
        match self.store.list_articles(&context.actor_id, &filter) {
            Ok(articles) => {
                let payload = articles
                    .into_iter()
                    .map(|article| serde_json::to_value(&article).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                Ok(ToolResult::ok(Value::Array(payload)).masked())
            }
            Err(error) => Ok(ToolResult::error("wiki_store_failed", error.to_string()).masked()),
        }
    }
}

struct UpdateWiki {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for UpdateWiki {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &["article_id", "title", "content", "category", "tags"],
        )?;
        let article_id = required_i64(&arguments, "article_id")?;
        let patch = WikiArticlePatch {
            title: optional_string(&arguments, "title")?,
            content: optional_string(&arguments, "content")?,
            category: optional_string(&arguments, "category")?,
            tags: optional_string_array(&arguments, "tags")?.map(Value::from),
            ..WikiArticlePatch::default()
        };
        match self
            .store
            .update_article(&context.actor_id, article_id, &patch)
        {
            Ok(article) => {
                Ok(ToolResult::ok(serde_json::to_value(&article).unwrap_or(Value::Null)).masked())
            }
            Err(error) => Ok(ToolResult::error("wiki_store_failed", error.to_string()).masked()),
        }
    }
}

struct RssList {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for RssList {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["category"])?;
        let category = optional_string(&arguments, "category")?;
        match self
            .store
            .list_subscriptions(&context.actor_id, category.as_deref(), None)
        {
            Ok(subscriptions) => {
                let payload = subscriptions
                    .into_iter()
                    .map(|subscription| serde_json::to_value(&subscription).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                Ok(ToolResult::ok(Value::Array(payload)).masked())
            }
            Err(error) => Ok(ToolResult::error("rss_store_failed", error.to_string()).masked()),
        }
    }
}

struct RssSubscribe {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for RssSubscribe {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["url", "category"])?;
        let new = NewRssSubscription {
            url: required_string(&arguments, "url")?.to_owned(),
            category: optional_string(&arguments, "category")?,
            ..NewRssSubscription::default()
        };
        match self.store.create_subscription(&context.actor_id, &new) {
            Ok(subscription) => Ok(ToolResult::ok(
                serde_json::to_value(&subscription).unwrap_or(Value::Null),
            )
            .masked()),
            Err(error) => Ok(ToolResult::error("rss_store_failed", error.to_string()).masked()),
        }
    }
}

struct RssUnsubscribe {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for RssUnsubscribe {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["subscription_id"])?;
        let subscription_id = required_i64(&arguments, "subscription_id")?;
        match self
            .store
            .delete_subscription(&context.actor_id, subscription_id)
        {
            Ok(()) => Ok(ToolResult::ok(json!({"subscription_id": subscription_id})).masked()),
            Err(error) => Ok(ToolResult::error("rss_store_failed", error.to_string()).masked()),
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

struct CreateTask {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for CreateTask {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &[
                "name",
                "prompt",
                "schedule",
                "template",
                "timezone",
                "model",
                "tools",
                "delivery_channels",
                "quiet_hours_start",
                "quiet_hours_end",
                "enabled",
            ],
        )?;
        let name = required_string(&arguments, "name")?.to_owned();
        // Python's `x or preset.x` treats an empty string/list as unset, so an
        // empty prompt/schedule falls back to the template instead of failing.
        let prompt = optional_string_lenient(&arguments, "prompt")?;
        let schedule = optional_string_lenient(&arguments, "schedule")?;
        let template = optional_string_lenient(&arguments, "template")?;
        let timezone = optional_string_lenient(&arguments, "timezone")?;
        let model = optional_string_lenient(&arguments, "model")?;
        let tools = optional_string_array(&arguments, "tools")?.filter(|values| !values.is_empty());
        let delivery_channels = optional_string_array(&arguments, "delivery_channels")?
            .filter(|values| !values.is_empty());
        let quiet_hours_start = optional_string_lenient(&arguments, "quiet_hours_start")?;
        let quiet_hours_end = optional_string_lenient(&arguments, "quiet_hours_end")?;
        let enabled = optional_bool(&arguments, "enabled")?.unwrap_or(true);

        let preset = match template.as_deref() {
            Some(slug) => {
                match cool_app_server::task_templates()
                    .into_iter()
                    .find(|preset| preset.slug == slug)
                {
                    Some(preset) => Some(preset),
                    None => {
                        return Ok(ToolResult::error(
                            "task_template_not_found",
                            format!(
                                "Unknown template {slug:?}. Available: news-digest, \
                                 code-review, memory-review, health-check."
                            ),
                        )
                        .masked());
                    }
                }
            }
            None => None,
        };
        // Python `prompt or preset.prompt`, then a required-prompt guard.
        let effective_prompt = prompt
            .or_else(|| preset.as_ref().map(|preset| preset.prompt.clone()))
            .unwrap_or_default();
        if effective_prompt.trim().is_empty() {
            return Ok(ToolResult::error(
                "task_prompt_required",
                "A task needs a prompt (or a template that provides one).",
            )
            .masked());
        }
        let cron = match resolve_schedule(schedule.as_deref()) {
            Ok(Some(cron)) => Some(cron),
            Ok(None) => preset.as_ref().map(|preset| preset.cron_expression.clone()),
            Err(message) => return Ok(ToolResult::error("invalid_schedule", message).masked()),
        };
        let Some(cron) = cron else {
            return Ok(ToolResult::error(
                "task_schedule_required",
                "A task needs a schedule: pass a cron expression or a phrase like \
                 'every day at 8pm'.",
            )
            .masked());
        };
        let tools = tools.or_else(|| {
            preset
                .as_ref()
                .and_then(|preset| preset.tools_whitelist.clone())
        });
        let delivery_channels = delivery_channels.or_else(|| {
            preset
                .as_ref()
                .map(|preset| preset.delivery_channels.clone())
        });
        let new = NewScheduledTask {
            name,
            trigger_type: cool_store::domains::tasks::TRIGGER_CRON.to_owned(),
            cron_expression: Some(cron.clone()),
            timezone: timezone.unwrap_or_else(|| "UTC".to_owned()),
            quiet_hours_start,
            quiet_hours_end,
            prompt: effective_prompt,
            workflow_type: preset.as_ref().map(|preset| preset.slug.clone()),
            model,
            tools_whitelist: tools.map(string_array_value),
            // Python passes the run context workdir so the task runs where it was
            // created; the store tools only know the run workspace.
            working_directory: Some(context.workspace.root().to_string_lossy().into_owned()),
            delivery_channels: delivery_channels.map(string_array_value),
            max_iterations: preset
                .as_ref()
                .map(|preset| preset.max_iterations)
                .unwrap_or(10),
            enabled,
            ..NewScheduledTask::default()
        };
        match self.store.create_task(&context.actor_id, &new) {
            Ok(task) => Ok(ToolResult::ok(json!({
                "created": task_summary(&task),
                "schedule_description": describe_cron(&cron),
            }))
            .masked()),
            Err(error) => Ok(ToolResult::error("task_store_failed", error.to_string()).masked()),
        }
    }
}

struct ListTasks {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for ListTasks {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["enabled_only"])?;
        let enabled_only = optional_bool(&arguments, "enabled_only")?.unwrap_or(false);
        match self.store.list_tasks(&context.actor_id, enabled_only) {
            Ok(tasks) => {
                let payload = tasks.iter().map(task_summary).collect::<Vec<_>>();
                Ok(ToolResult::ok(Value::Array(payload)).masked())
            }
            Err(error) => Ok(ToolResult::error("task_store_failed", error.to_string()).masked()),
        }
    }
}

struct UpdateTask {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for UpdateTask {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &[
                "task_id",
                "name",
                "prompt",
                "schedule",
                "timezone",
                "model",
                "tools",
                "delivery_channels",
                "enabled",
            ],
        )?;
        let task_id = required_i64(&arguments, "task_id")?;
        let mut patch = ScheduledTaskPatch::default();
        let mut touched = false;
        if let Some(value) = optional_string(&arguments, "name")? {
            patch.name = Some(value);
            touched = true;
        }
        if let Some(value) = optional_string(&arguments, "prompt")? {
            patch.prompt = Some(value);
            touched = true;
        }
        if let Some(value) = optional_string(&arguments, "timezone")? {
            patch.timezone = Some(value);
            touched = true;
        }
        if let Some(value) = optional_string(&arguments, "model")? {
            patch.model = Some(value);
            touched = true;
        }
        if let Some(value) = optional_string_array(&arguments, "tools")? {
            patch.tools_whitelist = Some(string_array_value(value));
            touched = true;
        }
        if let Some(value) = optional_string_array(&arguments, "delivery_channels")? {
            patch.delivery_channels = Some(string_array_value(value));
            touched = true;
        }
        if let Some(value) = optional_bool(&arguments, "enabled")? {
            patch.enabled = Some(value);
            touched = true;
        }
        if let Some(schedule) = optional_string(&arguments, "schedule")? {
            match resolve_schedule(Some(&schedule)) {
                Ok(Some(cron)) => {
                    patch.cron_expression = Some(cron);
                    patch.trigger_type = Some(cool_store::domains::tasks::TRIGGER_CRON.to_owned());
                    touched = true;
                }
                Ok(None) => {}
                Err(message) => {
                    return Ok(ToolResult::error("invalid_schedule", message).masked());
                }
            }
        }
        if !touched {
            return Ok(ToolResult::error(
                "nothing_to_update",
                "Nothing to update: pass at least one field.",
            )
            .masked());
        }
        match self.store.update_task(&context.actor_id, task_id, &patch) {
            Ok(task) => Ok(ToolResult::ok(json!({"updated": task_summary(&task)})).masked()),
            Err(StoreError::NotFound(_)) => Ok(ToolResult::error(
                "task_not_found",
                format!("Task {task_id} not found."),
            )
            .masked()),
            Err(error) => Ok(ToolResult::error("task_store_failed", error.to_string()).masked()),
        }
    }
}

struct DeleteTask {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for DeleteTask {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["task_id"])?;
        let task_id = required_i64(&arguments, "task_id")?;
        match self.store.delete_task(&context.actor_id, task_id) {
            Ok(()) => Ok(ToolResult::ok(json!({"deleted": task_id})).masked()),
            Err(StoreError::NotFound(_)) => Ok(ToolResult::error(
                "task_not_found",
                format!("Task {task_id} not found."),
            )
            .masked()),
            Err(error) => Ok(ToolResult::error("task_store_failed", error.to_string()).masked()),
        }
    }
}

struct ParseCron;

#[async_trait]
impl ToolHandler for ParseCron {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["text"])?;
        let text = required_string(&arguments, "text")?;
        match parse_natural_schedule(text) {
            Some(cron) => {
                let runs = cool_store::scheduler::cron_next_runs(&cron, now_seconds(), 3)
                    .unwrap_or_default();
                Ok(ToolResult::ok(json!({
                    "cron_expression": cron,
                    "description": describe_cron(&cron),
                    "next_runs_utc": runs.into_iter().map(format_run_time).collect::<Vec<_>>(),
                }))
                .masked())
            }
            None => Ok(ToolResult::error(
                "invalid_schedule",
                format!(
                    "Could not interpret {text:?} as a schedule. Try phrasings like \
                     'every day at 8pm', 'каждый вторник в 18:30', 'every 30 minutes'."
                ),
            )
            .masked()),
        }
    }
}

/// Turn cron-or-prose into a cron expression, mirroring Python
/// `task_tools._resolve_schedule`.
fn resolve_schedule(text: Option<&str>) -> Result<Option<String>, String> {
    let Some(text) = text.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if cool_store::scheduler::cron_next_runs(text, now_seconds(), 1).is_ok() {
        return Ok(Some(text.to_owned()));
    }
    match parse_natural_schedule(text) {
        Some(cron) => Ok(Some(cron)),
        None => Err(format!(
            "Could not interpret the schedule {text:?}. Provide a 5-field cron \
             expression (minute hour day-of-month month day-of-week) instead."
        )),
    }
}

/// Python `_task_summary` projection; `delivery_channels` defaults to `["ui"]`
/// (an empty list counts as unset, matching Python's `or ["ui"]`).
fn task_summary(task: &ScheduledTask) -> Value {
    let delivery_channels = match &task.delivery_channels {
        Some(Value::Array(items)) if !items.is_empty() => task.delivery_channels.clone(),
        _ => Some(json!(["ui"])),
    };
    json!({
        "id": task.id,
        "name": task.name,
        "trigger_type": task.trigger_type,
        "cron_expression": task.cron_expression,
        "schedule": task.cron_expression.as_deref().map(describe_cron),
        "timezone": task.timezone,
        "enabled": task.enabled,
        "next_run_at": task.next_run_at,
        "last_run_at": task.last_run_at,
        "last_status": task.last_status,
        "delivery_channels": delivery_channels.unwrap_or_else(|| json!(["ui"])),
    })
}

fn string_array_value(values: Vec<String>) -> Value {
    Value::Array(values.into_iter().map(Value::String).collect())
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn format_run_time(timestamp: i64) -> String {
    format!(
        "{}Z",
        cool_store::python_datetime(timestamp, 0).replace(' ', "T")
    )
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

/// Like [`optional_string`] but treats an empty string as absent (Python `x or
/// default`). Used where a value falls back to a template/default.
fn optional_string_lenient(arguments: &Value, name: &str) -> Result<Option<String>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok((!value.is_empty()).then(|| value.clone())),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "{name} must be a string"
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
