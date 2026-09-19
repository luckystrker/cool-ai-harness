//! Provider, budget, analytics, task and profile families.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::MessageRecord;
use crate::IdempotencyKey;

/// One provider row. The encrypted API key is intentionally absent.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProviderRecord {
    pub id: i64,
    pub name: String,
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub is_active: bool,
    pub is_subscription: bool,
    pub is_fallback: bool,
    pub is_default: bool,
    pub chat_models: Option<Value>,
}

/// One model advertised by a provider row's cached catalog.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelInfoRecord {
    pub id: String,
    pub context_window: Option<i64>,
    pub prompt_price: Option<f64>,
    pub completion_price: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProviderListParams {
    #[serde(default)]
    pub include_inactive: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProviderCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_subscription: bool,
    #[serde(default)]
    pub is_fallback: bool,
    #[serde(default)]
    pub is_default: bool,
    pub chat_models: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProviderUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub default_model: Option<String>,
    pub is_active: Option<bool>,
    pub is_fallback: Option<bool>,
    pub is_default: Option<bool>,
    pub chat_models: Option<Value>,
}

/// One budget configuration row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BudgetRecord {
    pub id: i64,
    pub user_id: i64,
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold_pct: f64,
    pub block_on_exceed: bool,
    pub override_until: Option<String>,
    pub last_alert_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Spend against one budget window.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BudgetWindowSpendRecord {
    pub spend_usd: f64,
    pub limit_usd: Option<f64>,
    pub pct: f64,
}

/// Live budget picture: config, window spend and derived status.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BudgetStatusRecord {
    pub status: String,
    pub overridden: bool,
    pub daily: BudgetWindowSpendRecord,
    pub weekly: BudgetWindowSpendRecord,
    pub monthly: BudgetWindowSpendRecord,
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold_pct: f64,
    pub block_on_exceed: bool,
    pub override_until: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BudgetUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold_pct: Option<f64>,
    pub block_on_exceed: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BudgetOverrideParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub until: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BudgetSpendParams {
    pub since: Option<String>,
    pub limit: u16,
}

/// One spend-log row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpendEntryRecord {
    pub id: i64,
    pub run_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub provider_name: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub ts: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Aggregated analytics counters.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnalyticsSummaryRecord {
    pub total_spend_usd: f64,
    pub total_llm_calls: i64,
    pub total_tokens: i64,
    pub total_tool_calls: i64,
    pub tool_error_count: i64,
    pub tool_success_rate: f64,
    pub days: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpendBucketRecord {
    pub period: String,
    pub cost_usd: f64,
    pub total_tokens: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelSpendRecord {
    pub model: String,
    pub cost_usd: f64,
    pub total_tokens: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolUsageRecord {
    pub name: String,
    pub calls: i64,
    pub avg_duration_ms: f64,
    pub success_rate: f64,
    pub error_count: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LatencyBucketRecord {
    pub period: String,
    pub avg_ms: f64,
    pub min_ms: i64,
    pub max_ms: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CallHistoryRowRecord {
    pub id: i64,
    pub ts: Option<String>,
    pub model: String,
    pub provider_name: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub run_id: Option<i64>,
    pub conversation_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CallHistoryResult {
    pub rows: Vec<CallHistoryRowRecord>,
    pub total: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemoryActivityBucketRecord {
    pub period: String,
    pub created: i64,
    pub by_type: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AnalyticsDaysParams {
    pub days: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AnalyticsBucketParams {
    pub days: u16,
    pub bucket: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AnalyticsTopToolsParams {
    pub days: u16,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AnalyticsCallHistoryParams {
    pub limit: u16,
    #[serde(default)]
    pub offset: u32,
    pub model: Option<String>,
    pub provider: Option<String>,
}

/// One scheduled task row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaskRecord {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub cron_expression: Option<String>,
    pub interval_seconds: Option<i64>,
    pub run_at: Option<String>,
    pub timezone: String,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub misfire_policy: String,
    pub prompt: String,
    pub workflow_type: Option<String>,
    pub profile_id: Option<i64>,
    pub model: Option<String>,
    pub tools_whitelist: Option<Value>,
    pub capability_policy: Option<Value>,
    pub working_directory: Option<String>,
    pub approval_policy: String,
    pub delivery_channels: Option<Value>,
    pub delivery_config: Option<Value>,
    pub last_delivery_hash: Option<String>,
    pub max_iterations: i64,
    pub max_cost_per_run: Option<f64>,
    pub timeout_s: Option<f64>,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub run_count: i64,
    pub failure_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One scheduled-task run row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaskRunRecord {
    pub id: i64,
    pub task_id: i64,
    pub conversation_id: Option<i64>,
    pub run_id: Option<i64>,
    pub status: String,
    pub trigger_source: String,
    pub prompt: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub skip_reason: Option<String>,
    pub approval_policy: Option<String>,
    pub approval_reason: Option<String>,
    pub usage: Option<Value>,
    pub duration_ms: Option<i64>,
    pub delivery_status: Option<Value>,
    pub delivered_at: Option<String>,
    pub is_read: bool,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Task run plus the transcript of its isolated conversation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaskRunDetailRecord {
    pub run: TaskRunRecord,
    pub messages: Vec<MessageRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaskInboxResult {
    pub unread_count: i64,
    pub runs: Vec<TaskRunRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SchedulerJobRecord {
    pub id: String,
    pub name: String,
    pub next_run_time: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SchedulerStatusRecord {
    pub enabled: bool,
    pub running: bool,
    pub timezone: String,
    pub max_concurrent_tasks: u32,
    pub jobs: Vec<SchedulerJobRecord>,
}

/// Parsed cron schedule plus the next fire times.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ParseCronResult {
    pub cron_expression: Option<String>,
    pub description: Option<String>,
    pub next_runs: Vec<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaskRunCancelResult {
    pub task_run_id: i64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TaskListParams {
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TaskCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub cron_expression: Option<String>,
    pub interval_seconds: Option<i64>,
    pub run_at: Option<String>,
    pub timezone: String,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub misfire_policy: String,
    pub prompt: String,
    pub workflow_type: Option<String>,
    pub profile_id: Option<i64>,
    pub model: Option<String>,
    pub tools_whitelist: Option<Value>,
    pub capability_policy: Option<Value>,
    pub working_directory: Option<String>,
    pub approval_policy: String,
    pub delivery_channels: Option<Value>,
    pub delivery_config: Option<Value>,
    pub max_iterations: i64,
    pub max_cost_per_run: Option<f64>,
    pub timeout_s: Option<f64>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TaskUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub trigger_type: Option<String>,
    pub cron_expression: Option<String>,
    pub interval_seconds: Option<i64>,
    pub run_at: Option<String>,
    pub timezone: Option<String>,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub misfire_policy: Option<String>,
    pub prompt: Option<String>,
    pub workflow_type: Option<String>,
    pub profile_id: Option<i64>,
    pub model: Option<String>,
    pub tools_whitelist: Option<Value>,
    pub capability_policy: Option<Value>,
    pub working_directory: Option<String>,
    pub approval_policy: Option<String>,
    pub delivery_channels: Option<Value>,
    pub delivery_config: Option<Value>,
    pub max_iterations: Option<i64>,
    pub max_cost_per_run: Option<f64>,
    pub timeout_s: Option<f64>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TaskRunsParams {
    pub task_id: i64,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TaskRunReadParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub is_read: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TaskInboxParams {
    pub unread_only: bool,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ParseCronParams {
    pub text: String,
}

/// One agent profile row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileRecord {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub skill_names: Option<Value>,
    pub settings: Option<Value>,
    pub avatar_color: Option<String>,
    pub is_builtin: bool,
    pub is_active: bool,
    pub is_shared: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProfileListParams {
    #[serde(default)]
    pub include_inactive: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProfileCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub skill_names: Option<Value>,
    pub settings: Option<Value>,
    pub avatar_color: Option<String>,
    #[serde(default)]
    pub is_builtin: bool,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_shared: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProfileUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub skill_names: Option<Value>,
    pub settings: Option<Value>,
    pub avatar_color: Option<String>,
    pub is_active: Option<bool>,
    pub is_shared: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProfilePlaygroundParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub title: Option<String>,
    pub initial_prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlaygroundResult {
    pub conversation_id: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SeedResult {
    pub created: u64,
}
