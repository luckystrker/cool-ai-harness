//! Memory, entity, wiki, RSS and webhook families.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::IdempotencyKey;

/// One long-term memory row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemoryRecord {
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryListParams {
    pub memory_type: Option<String>,
    pub scope: Option<String>,
    pub status: Option<String>,
    pub conversation_id: Option<i64>,
    pub pinned: Option<bool>,
    pub limit: u16,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub scope: Option<String>,
    pub agent_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub memory_type: Option<String>,
    pub content: String,
    pub structured: Option<Value>,
    pub tags: Option<Value>,
    pub importance: Option<f64>,
    pub confidence: Option<f64>,
    pub source: Option<String>,
    pub status: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    pub supersedes_id: Option<i64>,
    pub ttl_days: Option<i64>,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryDeleteParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    #[serde(default)]
    pub hard: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryPendingParams {
    pub limit: u16,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryPinParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub pinned: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MemoryEpisodesParams {
    pub agent_id: Option<i64>,
    pub limit: u16,
}

/// Score breakdown of why a memory is retained.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemoryScoreBreakdown {
    pub importance: f64,
    pub recency: f64,
    pub confidence: f64,
    pub type_priority: f64,
    pub age_days: f64,
    pub total: f64,
}

/// Explainability projection for one memory.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemoryExplainRecord {
    pub memory_id: i64,
    pub source: String,
    pub scope: String,
    pub status: String,
    pub pinned: bool,
    pub confidence: f64,
    pub importance: f64,
    pub memory_type: String,
    pub conversation_id: Option<i64>,
    pub agent_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub last_accessed_at: Option<String>,
    pub access_count: i64,
    pub score: MemoryScoreBreakdown,
}

/// Dashboard counters for the memory subsystem.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemoryStatsRecord {
    pub total_active: i64,
    pub by_type: BTreeMap<String, i64>,
    pub by_scope: BTreeMap<String, i64>,
    pub total_episodes: i64,
    pub total_archived: i64,
    pub total_pending: i64,
    pub total_entities: i64,
}

/// One episodic memory row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EpisodeRecord {
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

/// One entity row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityRecord {
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EntityListParams {
    pub entity_type: Option<String>,
    pub query: Option<String>,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EntityCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub entity_type: String,
    pub aliases: Option<Value>,
    pub attributes: Option<Value>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EntityUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub name: Option<String>,
    pub entity_type: Option<String>,
    pub aliases: Option<Value>,
    pub attributes: Option<Value>,
    pub description: Option<String>,
}

/// One wiki article row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WikiArticleRecord {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub category: String,
    pub tags: Option<Value>,
    pub source: String,
    pub source_memory_id: Option<i64>,
    pub user_id: Option<i64>,
    pub project_key: Option<String>,
    pub is_pinned: bool,
    pub is_archived: bool,
    pub version: i64,
    pub metadata: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WikiStatsRecord {
    pub total: i64,
    pub pinned: i64,
    pub archived: i64,
    pub by_category: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WikiListParams {
    pub category: Option<String>,
    pub tag: Option<String>,
    pub archived: Option<bool>,
    pub project_key: Option<String>,
    pub pinned: Option<bool>,
    pub search: Option<String>,
    pub limit: u16,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WikiSearchParams {
    pub query: String,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WikiCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub title: String,
    pub content: String,
    pub category: Option<String>,
    pub tags: Option<Value>,
    pub source: Option<String>,
    pub source_memory_id: Option<i64>,
    pub project_key: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WikiUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub title: Option<String>,
    pub content: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Value>,
    pub is_pinned: Option<bool>,
    pub is_archived: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WikiPromoteParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub memory_item_id: i64,
    pub title: String,
    pub content: String,
    pub category: Option<String>,
    pub tags: Option<Value>,
}

/// One RSS subscription row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RssSubscriptionRecord {
    pub id: i64,
    pub user_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub category: Option<String>,
    pub fetch_interval_minutes: i64,
    pub enabled: bool,
    pub last_fetched_at: Option<String>,
    pub last_error: Option<String>,
    pub entry_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One RSS entry row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RssEntryRecord {
    pub id: i64,
    pub subscription_id: i64,
    pub guid: String,
    pub title: Option<String>,
    pub link: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub published_at: Option<String>,
    pub content_hash: Option<String>,
    pub is_read: bool,
    pub fetched_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RssSubscriptionListParams {
    pub category: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RssSubscribeParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub category: Option<String>,
    pub fetch_interval_minutes: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RssEntriesParams {
    pub subscription_id: i64,
    pub unread_only: bool,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RssAllEntriesParams {
    pub unread_only: bool,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RssEntryReadParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub is_read: bool,
}

/// One webhook endpoint row. The HMAC `secret` is intentionally absent.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WebhookEndpointRecord {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub hook_id: String,
    pub source_type: String,
    pub event_filter: Option<Value>,
    pub task_id: Option<i64>,
    pub prompt_template: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// One webhook event row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WebhookEventRecord {
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WebhookCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub source_type: Option<String>,
    pub event_filter: Option<Value>,
    pub task_id: Option<i64>,
    pub prompt_template: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WebhookUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub name: Option<String>,
    pub source_type: Option<String>,
    pub event_filter: Option<Value>,
    pub task_id: Option<i64>,
    pub prompt_template: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WebhookEventsParams {
    pub endpoint_id: i64,
    pub status: Option<String>,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WebhookReplayParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub endpoint_id: i64,
    pub event_id: i64,
}
