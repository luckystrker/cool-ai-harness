//! Conversation, run, plan, subagent, research, artifact and workspace
//! families.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::IdempotencyKey;

/// One legacy conversation row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConversationRecord {
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

/// List/filter conversations.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConversationListParams {
    #[serde(default)]
    pub include_machine_owned: bool,
    pub archived: Option<bool>,
    pub pinned: Option<bool>,
    pub folder: Option<String>,
    pub search: Option<String>,
    pub limit: u16,
    #[serde(default)]
    pub offset: u32,
}

/// Create a conversation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConversationCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
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

/// Patch a conversation; `None` fields are left unchanged.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConversationUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub title: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub working_directory: Option<String>,
    pub permissions: Option<Value>,
    pub capability_policy: Option<Value>,
    pub profile_id: Option<i64>,
    pub tags: Option<Value>,
    pub folder: Option<String>,
    pub is_pinned: Option<bool>,
    pub is_archived: Option<bool>,
    pub metadata: Option<Value>,
}

/// Approval audit rows for one conversation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConversationApprovalsParams {
    pub id: i64,
    pub run_id: Option<i64>,
    pub limit: u16,
}

/// Full-text search over conversation message content.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConversationSearchParams {
    pub query: String,
    pub limit: u16,
}

/// Bulk operation over conversation ids.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConversationBulkParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub ids: Vec<i64>,
    pub action: String,
    pub folder: Option<String>,
}

/// Result of a conversation compaction.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CompactResult {
    pub status: String,
    pub reason: Option<String>,
    pub message_count: Option<u64>,
    pub messages_compacted: Option<u64>,
    pub messages_kept: Option<u64>,
    pub summary_length: Option<u64>,
}

/// One legacy message row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MessageRecord {
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

/// One approval audit row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApprovalAuditRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Option<Value>,
    pub approved: bool,
    pub decision_source: String,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
    pub is_breakpoint: bool,
    pub breakpoint_type: Option<String>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// One legacy agent run row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AgentRunRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub user_id: Option<i64>,
    pub status: String,
    pub model: Option<String>,
    pub config: Option<Value>,
    pub checkpoint: Option<Value>,
    pub usage: Option<Value>,
    pub iterations: i64,
    pub finish_reason: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// List runs of a conversation, newest first.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunListParams {
    pub conversation_id: i64,
    pub before_id: Option<i64>,
    pub limit: u16,
}

/// One legacy run event row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RunEventRecord {
    pub id: i64,
    pub run_id: i64,
    pub seq: i64,
    pub kind: String,
    pub payload: Option<Value>,
    pub created_at: String,
}

/// Paginated run-event read.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunEventsLegacyParams {
    pub run_id: i64,
    pub after_seq: Option<i64>,
    pub limit: u16,
}

/// One legacy tool-call row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolCallRecord {
    pub id: i64,
    pub conversation_id: Option<i64>,
    pub message_id: Option<i64>,
    pub user_id: Option<i64>,
    pub name: String,
    pub arguments: Option<Value>,
    pub result: Option<Value>,
    pub duration_ms: Option<i64>,
    pub success: bool,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One legacy plan row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlanRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub title: Option<String>,
    pub status: String,
    pub steps: Option<Value>,
    pub metadata: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// One plan step row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlanStepRecord {
    pub id: i64,
    pub plan_id: i64,
    pub position: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub depends_on: Option<Value>,
    pub tools: Option<Value>,
    pub result_summary: Option<String>,
    pub run_id: Option<i64>,
    pub delegate_role: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One plan template row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlanTemplateRecord {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub steps: Value,
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanListParams {
    pub conversation_id: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanIdParams {
    pub conversation_id: i64,
    pub plan_id: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct IdempotentPlanIdParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub conversation_id: i64,
    pub plan_id: i64,
}

/// Edit a draft plan's title and/or steps.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub conversation_id: i64,
    pub plan_id: i64,
    pub title: Option<String>,
    pub steps: Option<Value>,
}

/// Approve (or reject) a draft plan.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanApproveParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub conversation_id: i64,
    pub plan_id: i64,
    pub approved: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanTemplateCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub description: Option<String>,
    pub steps: Value,
}

/// One subagent role row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubagentRoleRecord {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub capability_policy: Option<Value>,
    pub max_iterations: i64,
    pub max_cost_usd: Option<f64>,
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentRoleCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub capability_policy: Option<Value>,
    pub max_iterations: i64,
    pub max_cost_usd: Option<f64>,
    #[serde(default)]
    pub is_builtin: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentRoleUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub capability_policy: Option<Value>,
    pub max_iterations: Option<i64>,
    pub max_cost_usd: Option<f64>,
    #[serde(default)]
    pub clear_max_cost_usd: bool,
}

/// One subagent run row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubagentRunRecord {
    pub id: i64,
    pub role_id: Option<i64>,
    pub profile_id: Option<i64>,
    pub parent_conversation_id: i64,
    pub parent_run_id: Option<i64>,
    pub research_run_id: Option<i64>,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub name: Option<String>,
    pub prompt: String,
    pub status: String,
    pub result_summary: Option<String>,
    pub usage: Option<Value>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Launch one subagent run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentLaunchParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub parent_conversation_id: i64,
    pub role_id: Option<i64>,
    pub profile_id: Option<i64>,
    pub name: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentLaunchItem {
    pub role_id: Option<i64>,
    pub profile_id: Option<i64>,
    pub name: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentLaunchBatchParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub parent_conversation_id: i64,
    pub items: Vec<SubagentLaunchItem>,
}

/// Result of cancelling a subagent run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubagentRunCancelResult {
    pub run_id: i64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentRunListParams {
    pub parent_conversation_id: Option<i64>,
    pub status: Option<String>,
    pub limit: u16,
}

/// Subagent run plus its isolated transcript.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SubagentRunDetailRecord {
    pub run: SubagentRunRecord,
    pub messages: Vec<MessageRecord>,
}

/// One deep-research run row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResearchRunRecord {
    pub id: i64,
    pub user_id: i64,
    pub conversation_id: Option<i64>,
    pub parent_task_run_id: Option<i64>,
    pub topic: String,
    pub depth: i64,
    pub model: Option<String>,
    pub status: String,
    pub sub_questions: Option<Value>,
    pub sources: Option<Value>,
    pub citations: Option<Value>,
    pub report_markdown: Option<String>,
    pub report_artifact_id: Option<i64>,
    pub usage: Option<Value>,
    pub error: Option<String>,
    pub input_hash: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Research run with the report payload the UI renders.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResearchRunDetailRecord {
    pub run: ResearchRunRecord,
    pub conversation_id: Option<i64>,
    pub parent_task_run_id: Option<i64>,
    pub sub_questions: Vec<String>,
    pub sources: Vec<Value>,
    pub citations: Vec<Value>,
    pub report_markdown: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchListParams {
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub topic: String,
    pub depth: i64,
    pub model: Option<String>,
    pub conversation_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchRerunParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub depth: Option<i64>,
    pub model: Option<String>,
}

/// One artifact metadata row.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArtifactRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub tool_call_id: Option<String>,
    pub filename: String,
    pub media_type: String,
    pub kind: String,
    pub size_bytes: i64,
    pub sha256: Option<String>,
    pub storage_path: String,
    pub version: i64,
    pub parent_id: Option<i64>,
    pub metadata: Option<Value>,
    pub extracted_text: Option<String>,
    pub is_deleted: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Artifact metadata plus extracted text and version chain.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArtifactDetailRecord {
    pub artifact: ArtifactRecord,
    pub extracted_text: Option<String>,
    pub versions: Vec<ArtifactRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ArtifactListParams {
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub kind: Option<String>,
    pub include_deleted: bool,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ArtifactIdParams {
    pub conversation_id: i64,
    pub artifact_id: i64,
}

/// One projected inspector timeline event.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TimelineEntryRecord {
    pub index: u64,
    pub kind: String,
    pub phase: String,
    pub status: Option<String>,
    pub title: Option<String>,
    pub occurred_at: String,
    pub duration_ms: Option<i64>,
    pub payload: Option<Value>,
}

/// Inspector timeline for a run, rebuilt from the append-only event log.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TimelineRecord {
    pub run_id: i64,
    pub conversation_id: i64,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub total_duration_ms: Option<i64>,
    pub entries: Vec<TimelineEntryRecord>,
    pub usage: Option<Value>,
    pub error: Option<String>,
}

/// Metric deltas between two runs (always `right - left`).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ComparisonDeltasRecord {
    pub duration_ms: Option<i64>,
    pub event_count: i64,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub tool_calls: i64,
}

/// Side-by-side comparison of two runs.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RunComparisonRecord {
    pub left: TimelineRecord,
    pub right: TimelineRecord,
    pub deltas: ComparisonDeltasRecord,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct InspectorCompareParams {
    pub left_run_id: i64,
    pub right_run_id: i64,
}

/// Prepare a replay run from an existing run with optional overrides.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ReplayParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub run_id: i64,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
}

/// Result of preparing a replay run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReplayResult {
    pub new_run_id: i64,
    pub original_run_id: i64,
    pub status: String,
}

/// Result of cancelling a research run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResearchCancelResult {
    pub cancelled: i64,
}

/// Current git branch of a directory.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitInfoRecord {
    pub path: String,
    pub is_git: bool,
    pub branch: Option<String>,
}

/// Sub-directories of a path for the folder browser.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DirectoryListingRecord {
    pub current: String,
    pub parent: Option<String>,
    pub directories: Vec<String>,
    pub default: String,
}

/// Recently used working directories and the global default.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecentDirectoriesRecord {
    pub recent: Vec<String>,
    pub default: String,
}

/// Parsed git status.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitStatusRecord {
    pub path: String,
    pub is_git: bool,
    pub branch: Option<String>,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitLogEntryRecord {
    pub hash: String,
    pub message: String,
    pub author: String,
    pub date: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitLogRecord {
    pub path: String,
    pub commits: Vec<GitLogEntryRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitBranchesRecord {
    pub path: String,
    pub branches: Vec<String>,
    pub current: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GitCheckoutResult {
    pub path: String,
    pub branch: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkspacePathParams {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkspaceOptionalPathParams {
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkspaceGitLogParams {
    pub path: String,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkspaceGitCheckoutParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub path: String,
    pub branch: String,
}

/// One macro-tool row (Agent Constructor).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MacroToolRecord {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub steps: Value,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ConstructorMacroListParams {
    #[serde(default)]
    pub include_inactive: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MacroCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub steps: Value,
    #[serde(default)]
    pub is_active: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MacroUpdateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
    pub steps: Option<Value>,
    pub is_active: Option<bool>,
}
