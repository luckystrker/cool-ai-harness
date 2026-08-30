/** Types mirroring the backend Pydantic schemas (app/api/schemas.py).

 * Keep in sync when adding fields server-side. In Фаза 6 we can generate
 * these from the OpenAPI spec (openapi-typescript), but hand-written is
 * fine for the MVP. */

export interface HealthResponse {
  status: string
  version: string
  environment: string
}

// --- conversations ---

export type ToolPermission = "allow" | "ask" | "deny"

/** Tool permission map: tool name (or "*" wildcard) -> decision. */
export type ToolPermissions = Record<string, ToolPermission>

/** Capability names matching the backend Capability enum. */
export type CapabilityName =
  | "read"
  | "write"
  | "execute"
  | "network"
  | "git"
  | "send_external"

/** Capability policy: capability name (or "*" wildcard) -> decision. */
export type CapabilityPolicy = Record<string, ToolPermission>

/** Breakpoint type — when in the tool-call chain a breakpoint fires. */
export type BreakpointType =
  | "before_tool"
  | "after_tool_result"
  | "before_send"
  | "before_write"

/** A single breakpoint rule. */
export interface BreakpointConfig {
  type: BreakpointType
  /** If set, only fire for this specific tool. Undefined = any tool. */
  tool?: string
  /** TTL in seconds before fallback. */
  ttl_s?: number
  /** Fallback action on timeout: "deny" or "skip". */
  fallback?: "deny" | "skip"
}

export interface Conversation {
  id: number
  user_id: number
  title: string | null
  model: string | null
  /** Per-conversation working directory (overrides the global default). */
  working_directory: string | null
  /** Per-conversation tool permissions (override global defaults). */
  permissions: ToolPermissions | null
  /** Per-conversation capability policy (override global defaults). */
  capability_policy: CapabilityPolicy | null
  /** Per-conversation breakpoints (stored in metadata). */
  breakpoints: BreakpointConfig[] | null
  /** Active agent profile (Фаза 3a §2). Null = default system prompt. */
  profile_id: number | null
  /** Conversation organization (Фаза 3a §4). */
  tags: string[]
  folder: string | null
  is_pinned: boolean
  is_archived: boolean
  created_at: string
  updated_at: string
}

export interface ConversationCreate {
  title?: string
  system_prompt?: string
  model?: string
  tool_names?: string[]
  working_directory?: string
  permissions?: ToolPermissions
  capability_policy?: CapabilityPolicy
  breakpoints?: BreakpointConfig[]
  profile_id?: number
}

/** PATCH /api/conversations/{id} — only provided fields are applied. */
export interface ConversationUpdate {
  title?: string
  model?: string
  working_directory?: string
  permissions?: ToolPermissions
  capability_policy?: CapabilityPolicy
  breakpoints?: BreakpointConfig[]
  profile_id?: number
}

/** One row of a stored message. Matches app/api/schemas.MessageOut. */
export interface Message {
  id: number
  conversation_id: number
  role: "system" | "user" | "assistant" | "tool"
  content: string | null
  artifact_ids?: number[] | null
  tool_calls?: ToolCall[] | null
  usage?: Record<string, unknown> | null
  /** Reasoning / chain-of-thought (assistant messages), when the provider exposes one. */
  thinking?: string | null
  /** Structured tool result (role="tool" messages). */
  tool_result?: { tool_call_id?: string | null; name?: string | null; result?: ToolResultPayload } | null
  /** Which model produced this assistant message (snapshot at turn time). */
  model?: string | null
  /** Wall-clock duration of the agent turn that produced this message (ms). */
  duration_ms?: number | null
  created_at: string
}

/** Shape of the `result` object inside a tool_result event / tool_result row. */
export interface ToolResultPayload {
  output?: string
  is_error?: boolean
  error?: string | null
  metadata?: Record<string, unknown>
}

export interface ToolCall {
  id?: string | null
  type?: string
  name: string
  arguments: Record<string, unknown>
}

export interface ConversationDetail extends Conversation {
  messages: Message[]
  /** Rolling summary of compacted older messages (null when not compacted). */
  compact_summary?: string | null
  /** Messages with id <= this are covered by compact_summary. */
  compact_up_to_message_id?: number | null
}

export interface SendMessageRequest {
  content: string
  artifact_ids?: number[]
  model?: string
  system_prompt?: string
  tool_names?: string[]
  /** When true, the agent generates a structured plan instead of executing directly (Фаза 2 §1). */
  plan_mode?: boolean
}

// --- providers ---

export interface Provider {
  id: number
  name: string
  label: string | null
  base_url: string | null
  default_model: string | null
  is_active: boolean
  is_subscription: boolean
  /** Use as the backup provider when the primary is unhealthy (Фаза 1.5 §5). */
  is_fallback: boolean
  /** Marked as the default provider for new conversations (mutually exclusive). */
  is_default: boolean
  /** Model ids exposed in the chat model picker (selected in provider settings). */
  chat_models: string[]
  /** Masked preview like "sk-…cdef"; never the full secret. */
  api_key_hint: string | null
}

export interface ProviderCreate {
  name: string
  label?: string
  base_url?: string
  api_key: string
  default_model?: string
  is_subscription?: boolean
  is_fallback?: boolean
  chat_models?: string[]
  is_default?: boolean
}

export interface ProviderUpdate {
  label?: string
  base_url?: string
  api_key?: string
  default_model?: string
  is_active?: boolean
  is_fallback?: boolean
  chat_models?: string[]
  is_default?: boolean
}

/** One model a provider serves, with whatever metadata the provider returned. */
export interface ModelInfo {
  id: string
  context_window: number | null
  /** Per-1k-token USD price (prompt). null when unknown. */
  prompt_price: number | null
  /** Per-1k-token USD price (completion). null when unknown. */
  completion_price: number | null
}

/** Request body for POST /providers/models/preview (unsaved-provider probe). */
export interface ModelsPreviewRequest {
  name: string
  base_url?: string
  api_key: string
}

// --- agent events (streamed from SSE / WebSocket) ---

export type AgentEventKind =
  | "start"
  | "thinking"
  | "token"
  | "tool_call_start"
  | "tool_call_delta"
  | "tool_approval_request"
  | "tool_result"
  | "message"
  | "finish"
  | "error"
  // Cost budgets (Фаза 1.5 §5)
  | "budget_alert"
  // ReAct lifecycle events
  | "react_thought"
  | "react_action"
  | "react_observation"
  // Inspector per-iteration metrics (Фаза 1.5 §6)
  | "llm_call_complete"
  // Planning Mode (Фаза 2 §1)
  | "plan_generated"
  | "plan_step_start"
  | "plan_step_complete"
  | "plan_progress"
  // Subagents (Фаза 2 §5)
  | "subagent_started"
  | "subagent_progress"
  | "subagent_completed"
  | "subagent_failed"

/** Payload shape for a tool_approval_request event. */
export interface ToolApprovalRequestPayload {
  id: string
  name: string
  arguments: Record<string, unknown>
  reason: string
  requires_decision: true
  /** True when this was triggered by a breakpoint (vs a regular "ask" tool). */
  is_breakpoint?: boolean
  /** Breakpoint type, if is_breakpoint is true. */
  breakpoint_type?: BreakpointType
  /** Result preview (for after_tool_result breakpoints). */
  result_preview?: string
  /** Current file content before the write (for diff/preview in write tools). */
  current_content?: string
}

export interface AgentEvent {
  kind: AgentEventKind
  payload: Record<string, unknown>
}

export interface UsagePayload {
  prompt_tokens?: number
  completion_tokens?: number
  total_tokens?: number
  cost_usd?: number | null
}

// --- ReAct trace (Thought → Action → Observation) ---

export interface ReActThought {
  step: number
  text: string
}

export interface ReActAction {
  step: number
  tool_name: string
  arguments: Record<string, unknown>
  call_id: string
}

export interface ReActObservation {
  step: number
  tool_name: string
  result_summary: string
  is_error: boolean
}

/** A single ReAct step groups thought + actions + observations. */
export interface ReActStep {
  step: number
  thought?: string
  actions: ReActAction[]
  observations: ReActObservation[]
}

// --- System prompt settings ---

export interface SystemPromptResponse {
  prompt: string
  is_custom: boolean
  source: "inline" | "file" | "builtin"
}

export interface SystemPromptUpdate {
  prompt: string
}

// --- approval audit (Фаза 1.5 §2) ---

/** One row of the approval audit trail. Matches app/api/schemas.ApprovalAuditOut. */
export interface ApprovalAudit {
  id: number
  conversation_id: number
  run_id: number | null
  call_id: string
  tool_name: string
  arguments: Record<string, unknown> | null
  approved: boolean
  decision_source: string
  decided_by: string | null
  reason: string | null
  is_breakpoint: boolean
  breakpoint_type: string | null
  duration_ms: number | null
  created_at: string
}

// --- artifacts (Фаза 1.5 §3) ---

export type ArtifactKind =
  | "file"
  | "image"
  | "document"
  | "code"
  | "report"
  | "audio"
  | "tool_result"

/** Matches app/api/schemas.ArtifactOut. */
export interface Artifact {
  id: number
  conversation_id: number
  run_id: number | null
  tool_call_id: string | null
  filename: string
  media_type: string
  kind: ArtifactKind
  size_bytes: number
  sha256: string | null
  version: number
  parent_id: number | null
  metadata_: Record<string, unknown> | null
  created_at: string
  updated_at: string
}

/** Matches app/api/schemas.ArtifactDetail. */
export interface ArtifactDetail extends Artifact {
  extracted_text: string | null
  versions: Artifact[]
}

/** Matches app/api/schemas.ArtifactUploadResponse. */
export interface ArtifactUploadResponse {
  artifact: Artifact
  message: string
}

// --- budgets (Фаза 1.5 §5) ---

export type BudgetStatus = "ok" | "alert" | "blocked"
export type BudgetWindow = "daily" | "weekly" | "monthly"

export interface BudgetWindowSpend {
  spend_usd: number
  limit_usd: number | null
  pct: number
}

export interface BudgetStatusResponse {
  status: BudgetStatus
  overridden: boolean
  daily: BudgetWindowSpend
  weekly: BudgetWindowSpend
  monthly: BudgetWindowSpend
  daily_limit_usd: number | null
  weekly_limit_usd: number | null
  monthly_limit_usd: number | null
  alert_threshold_pct: number
  block_on_exceed: boolean
  override_until: string | null
}

export interface BudgetUpdate {
  daily_limit_usd?: number | null
  weekly_limit_usd?: number | null
  monthly_limit_usd?: number | null
  alert_threshold_pct?: number
  block_on_exceed?: boolean
}

export interface SpendRow {
  id: number
  run_id: number | null
  conversation_id: number | null
  provider_name: string
  model: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cost_usd: number
  ts: string
}

/** Payload shape for a budget_alert agent event. */
export interface BudgetAlertPayload {
  window: BudgetWindow
  spend_usd: number
  limit_usd: number
  pct: number
}

// --- agent runs (Фаза 1.5 §1 — durable runs) ---

/** Matches app/api/schemas.RunEventOut. */
export interface RunEventOut {
  id: number
  run_id: number
  seq: number
  kind: string
  payload: Record<string, unknown> | null
  created_at: string
}

/** Matches app/api/schemas.RunOut. */
export interface RunOut {
  id: number
  conversation_id: number
  status: string
  model: string | null
  iterations: number
  usage: Record<string, unknown> | null
  finish_reason: string | null
  error: string | null
  started_at: string
  finished_at: string | null
  created_at: string
  updated_at: string
}

/** Matches app/api/schemas.RunDetail. */
export interface RunDetail extends RunOut {
  config: Record<string, unknown> | null
  checkpoint: Record<string, unknown> | null
  events: RunEventOut[]
}

// --- inspector (Фаза 1.5 §6 — Debug / Inspector Mode) ---

/** Per-iteration detail reconstructed from the event log. */
export interface IterationDetail {
  iteration: number
  duration_ms: number | null
  usage: Record<string, unknown> | null
  model: string | null
  tool_calls: Record<string, unknown>[]
  finish_reason: string | null
}

/** Full run timeline with per-iteration breakdown. */
export interface RunTimeline {
  run: RunDetail
  iterations: IterationDetail[]
  total_duration_ms: number | null
}

/** Side-by-side comparison of two runs. */
export interface RunComparison {
  run_a: RunOut
  run_b: RunOut
  delta_tokens: number
  delta_cost_usd: number | null
  delta_iterations: number
  delta_duration_ms: number | null
  iterations_a: IterationDetail[]
  iterations_b: IterationDetail[]
}

/** Request body for POST .../runs/{id}/replay. */
export interface ReplayRequest {
  model?: string
  system_prompt?: string
  temperature?: number
}

/** Response from the replay endpoint. */
export interface ReplayResponse {
  new_run_id: number
  original_run_id: number
  status: string
}

// --- planning mode (Фаза 2 §1) ---

export type PlanStatus =
  | "draft"
  | "approved"
  | "executing"
  | "completed"
  | "failed"
  | "cancelled"

export type PlanStepStatus = "pending" | "running" | "completed" | "failed" | "skipped"

export interface PlanStep {
  position: number
  title: string
  description?: string | null
  status: PlanStepStatus
  depends_on?: number[] | null
  tools?: string[] | null
  delegate_role?: string | null
  result_summary?: string | null
}

export interface Plan {
  id: number
  conversation_id: number
  run_id: number | null
  title: string | null
  status: PlanStatus
  steps: PlanStep[]
  created_at: string
  updated_at: string
}

export interface PlanTemplate {
  id: number
  name: string
  description: string | null
  steps: PlanStep[]
  is_builtin: boolean
  created_at: string
  updated_at: string
}

/** Payload shape for a plan_generated agent event. */
export interface PlanGeneratedPayload {
  plan_id: number
  title: string | null
  steps: PlanStep[]
}

/** Payload shape for plan_step_start / plan_step_complete events. */
export interface PlanStepEventPayload {
  position: number
  title?: string
  status?: PlanStepStatus
  result_summary?: string | null
}

/** Payload shape for a plan_progress event. */
export interface PlanProgressPayload {
  completed: number
  total: number
  current_step: number | null
}

// --- skills ---

export interface SkillInfo {
  name: string
  description: string
  source: "builtin" | "user" | "plugin"
  tags: string[]
  tools: string[]
  version: string
  body: string
}

export interface SkillListResponse {
  skills: SkillInfo[]
}

export interface SkillCreateRequest {
  name: string
  description?: string
  tags?: string[]
  tools?: string[]
  body: string
  scope?: "global" | "user"
}

export interface SkillCreateResponse {
  name: string
  path: string
  scope: string
}

// --- MCP servers (Фаза 2 §4) ---

export type MCPTransportType = "stdio" | "http"

export type MCPServerStatusType = "disconnected" | "connecting" | "connected" | "error"

export interface MCPToolInfo {
  name: string
  qualified_name: string
  description: string
  server_name: string
  input_schema: Record<string, unknown>
}

export interface MCPServer {
  name: string
  transport: MCPTransportType
  status: MCPServerStatusType
  enabled: boolean
  description: string
  command: string
  args: string[]
  url: string
  capabilities: string[]
  timeout_s: number
  error: string | null
  tools: MCPToolInfo[]
  server_info: Record<string, unknown>
}

export interface MCPServerListResponse {
  servers: MCPServer[]
}

export interface MCPServerCreate {
  name: string
  transport?: MCPTransportType
  command?: string
  args?: string[]
  env?: Record<string, string>
  url?: string
  headers?: Record<string, string>
  enabled?: boolean
  description?: string
  capabilities?: string[]
  timeout_s?: number
}

export interface MCPServerUpdate {
  transport?: MCPTransportType
  command?: string
  args?: string[]
  env?: Record<string, string>
  url?: string
  headers?: Record<string, string>
  enabled?: boolean
  description?: string
  capabilities?: string[]
  timeout_s?: number
}

export interface MCPConnectResponse {
  name: string
  status: string
  tools_count: number
  error: string | null
}

export interface MCPHealthResponse {
  name: string
  healthy: boolean
}

export interface MCPToolListResponse {
  tools: MCPToolInfo[]
}

// --- MCP Store / Marketplace ---

export interface MCPStoreItem {
  name: string
  description: string
  version: string
  repository_url: string
  install_command: string
  transport: string
  packages_count: number
}

export interface MCPStoreSearchResponse {
  results: MCPStoreItem[]
  query: string
}

export interface MCPStoreInstallRequest {
  registry_name: string
  local_name?: string
}

export interface MCPStoreInstallResponse {
  name: string
  status: string
  tools_count: number
  error: string | null
}

// --- subagents (Фаза 2 §5) ---

export type SubagentRunStatus = "queued" | "running" | "completed" | "failed" | "cancelled"

export interface SubagentRole {
  id: number
  name: string
  description: string | null
  system_prompt: string | null
  model: string | null
  tool_names: string[] | null
  capability_policy: Record<string, string> | null
  max_iterations: number
  max_cost_usd: number | null
  is_builtin: boolean
  created_at: string
  updated_at: string
}

export interface SubagentRoleCreate {
  name: string
  description?: string
  system_prompt?: string
  model?: string
  tool_names?: string[]
  capability_policy?: Record<string, string>
  max_iterations?: number
  max_cost_usd?: number | null
}

export interface SubagentRoleUpdate {
  name?: string
  description?: string
  system_prompt?: string
  model?: string
  tool_names?: string[]
  capability_policy?: Record<string, string>
  max_iterations?: number
  max_cost_usd?: number | null
}

export interface SubagentRun {
  id: number
  role_id: number | null
  parent_conversation_id: number
  parent_run_id: number | null
  conversation_id: number
  run_id: number | null
  name: string | null
  prompt: string
  status: SubagentRunStatus
  result_summary: string | null
  usage: Record<string, unknown> | null
  error: string | null
  started_at: string
  finished_at: string | null
  created_at: string
  updated_at: string
}

export interface SubagentRunDetail extends SubagentRun {
  messages: Message[]
}

export interface SubagentLaunchRequest {
  prompt: string
  role_id?: number
  parent_conversation_id: number
  name?: string
  model?: string
}

export interface SubagentLaunchBatchItem {
  prompt: string
  role_id?: number
  name?: string
  model?: string
}

export interface SubagentLaunchBatchRequest {
  parent_conversation_id: number
  items: SubagentLaunchBatchItem[]
}

/** Payload shape for subagent_started events. */
export interface SubagentStartedPayload {
  subagent_run_id: number
  name: string | null
  role: string | null
  prompt: string
}

/** Payload shape for subagent_completed events. */
export interface SubagentCompletedPayload {
  subagent_run_id: number
  result_summary: string | null
  usage: Record<string, unknown> | null
}

/** Payload shape for subagent_failed events. */
export interface SubagentFailedPayload {
  subagent_run_id: number
  error: string
}

// --- memory (Фаза 3a) ---

export type MemoryType = "semantic" | "episodic" | "procedural" | "preference"
export type MemoryScope = "global" | "agent" | "conversation"
export type MemoryStatus =
  | "active"
  | "archived"
  | "superseded"
  | "deleted"
  | "pending_confirmation"

export interface MemoryItem {
  id: number
  user_id: number
  scope: MemoryScope
  agent_id: number | null
  conversation_id: number | null
  memory_type: MemoryType
  content: string
  structured: Record<string, unknown> | null
  tags: string[] | null
  importance: number
  confidence: number
  source: string
  status: MemoryStatus
  pinned: boolean
  access_count: number
  created_at: string
  updated_at: string
}

export interface MemoryCreate {
  content: string
  memory_type?: MemoryType
  scope?: MemoryScope
  agent_id?: number
  importance?: number
  confidence?: number
  tags?: string[]
  structured?: Record<string, unknown>
  ttl_days?: number
}

export interface MemoryUpdate {
  content?: string
  memory_type?: MemoryType
  scope?: MemoryScope
  importance?: number
  confidence?: number
  status?: MemoryStatus
  tags?: string[]
  structured?: Record<string, unknown>
  ttl_days?: number
  valid_to?: string
  pinned?: boolean
}

export interface Episode {
  id: number
  user_id: number
  agent_id: number | null
  conversation_id: number | null
  title: string
  summary: string
  outcome: string
  importance: number
  tags: string[] | null
  created_at: string
}

export interface MemoryStats {
  total_active: number
  by_type: Record<string, number>
  by_scope: Record<string, number>
  total_episodes: number
  total_archived: number
  total_pending: number
  total_entities: number
}

export interface MemoryExtractRequest {
  conversation_id: number
}

export interface MemoryExtractResponse {
  status: string
  stored_count: number
  detail: string | null
}

// "Why is this remembered" explanation (score breakdown + provenance).
export interface MemoryScoreBreakdown {
  importance: number
  recency: number
  confidence: number
  type_priority: number
  age_days: number
  total: number
}

export interface MemoryExplain {
  memory_id: number
  source: string
  scope: MemoryScope
  status: MemoryStatus
  pinned: boolean
  confidence: number
  importance: number
  memory_type: MemoryType
  conversation_id: number | null
  agent_id: number | null
  created_at: string
  updated_at: string
  last_accessed_at: string | null
  access_count: number
  score: MemoryScoreBreakdown
}

// --- Entity memory ---

export interface Entity {
  id: number
  user_id: number
  name: string
  entity_type: string
  aliases: string[] | null
  attributes: Record<string, unknown> | null
  description: string | null
  created_at: string
  updated_at: string
}

export interface EntityCreate {
  name: string
  entity_type?: string
  aliases?: string[] | null
  attributes?: Record<string, unknown> | null
  description?: string | null
}

export interface EntityUpdate {
  name?: string
  entity_type?: string
  aliases?: string[] | null
  attributes?: Record<string, unknown> | null
  description?: string | null
}

// --- Analytics dashboard (Фаза 3a §5) ---

export interface AnalyticsSummary {
  total_spend_usd: number
  total_llm_calls: number
  total_tokens: number
  total_tool_calls: number
  tool_error_count: number
  tool_success_rate: number
  days: number
}

export interface SpendTimeSeriesPoint {
  period: string
  cost_usd: number
  total_tokens: number
  calls: number
}

export interface ModelSpend {
  model: string
  cost_usd: number
  total_tokens: number
  calls: number
}

export interface TopTool {
  name: string
  calls: number
  avg_duration_ms: number
  success_rate: number
  error_count: number
}

export interface LatencyPoint {
  period: string
  avg_ms: number
  min_ms: number
  max_ms: number
  calls: number
}

export interface CallHistoryRow {
  id: number
  ts: string | null
  model: string
  provider_name: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cost_usd: number
  run_id: number | null
  conversation_id: number | null
}

export interface CallHistoryResponse {
  rows: CallHistoryRow[]
  total: number
}

export interface MemoryActivityPoint {
  period: string
  created: number
  by_type: Record<string, number>
}

// --- agent profiles (Фаза 3a §2) ---

export interface AgentProfile {
  id: number
  name: string
  slug: string
  description: string | null
  system_prompt: string | null
  model: string | null
  tool_names: string[] | null
  skill_names: string[] | null
  settings: Record<string, unknown> | null
  avatar_color: string | null
  is_builtin: boolean
  is_active: boolean
  is_shared: boolean
  created_at: string
  updated_at: string
}

export interface ProfileCreate {
  name: string
  slug: string
  description?: string
  system_prompt?: string
  model?: string
  tool_names?: string[]
  skill_names?: string[]
  settings?: Record<string, unknown>
  avatar_color?: string
  is_shared?: boolean
}

export interface ProfileUpdate {
  name?: string
  slug?: string
  description?: string
  system_prompt?: string
  model?: string
  tool_names?: string[]
  skill_names?: string[]
  settings?: Record<string, unknown>
  avatar_color?: string
  is_active?: boolean
  is_shared?: boolean
}

export interface ToolCatalogItem {
  name: string
  description: string
  dangerous: boolean
  capabilities: string[]
  parameters: Record<string, unknown>
  is_macro: boolean
}

export interface MacroStep {
  id: string
  tool_name: string
  arguments: Record<string, unknown>
}

export interface MacroTool {
  id: number
  name: string
  description: string
  input_schema: Record<string, unknown>
  steps: MacroStep[]
  is_active: boolean
  created_at: string
  updated_at: string
}

export interface MacroToolCreate {
  name: string
  description?: string
  input_schema?: Record<string, unknown>
  steps: MacroStep[]
}

// --- Wiki / Knowledge Base (Фаза 3a §3) ---

export interface WikiArticle {
  id: number
  title: string
  content: string
  category: string
  tags: string[]
  source: string
  source_memory_id?: number | null
  is_pinned: boolean
  is_archived: boolean
  version: number
  created_at: string
  updated_at: string
}

// --- Recurring tasks / cron jobs (Фаза 3b) ---

export type TaskTriggerType = "cron" | "interval" | "date"
export type TaskRunStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "skipped"
export type TaskTriggerSource = "schedule" | "manual" | "agent"
export type TaskMisfirePolicy = "skip" | "run"
export type TaskApprovalPolicy = "deny_external" | "allow_all"
export type TaskDeliveryChannel = "ui" | "webhook" | "telegram" | "email"

export interface ScheduledTask {
  id: number
  user_id: number
  name: string
  description: string | null
  trigger_type: TaskTriggerType
  cron_expression: string | null
  interval_seconds: number | null
  run_at: string | null
  timezone: string
  quiet_hours_start: string | null
  quiet_hours_end: string | null
  misfire_policy: TaskMisfirePolicy
  prompt: string
  workflow_type: string | null
  profile_id: number | null
  model: string | null
  tools_whitelist: string[] | null
  capability_policy: Record<string, string> | null
  working_directory: string | null
  approval_policy: TaskApprovalPolicy
  delivery_channels: TaskDeliveryChannel[] | null
  delivery_config: Record<string, unknown> | null
  max_iterations: number
  max_cost_per_run: number | null
  timeout_s: number | null
  enabled: boolean
  next_run_at: string | null
  last_run_at: string | null
  last_status: TaskRunStatus | null
  run_count: number
  failure_count: number
  created_at: string
  updated_at: string
  /** Derived server-side: human-readable schedule + upcoming fire times (UTC). */
  schedule_description: string | null
  next_runs: string[]
}

export interface ScheduledTaskCreate {
  name: string
  prompt?: string
  description?: string
  trigger_type?: TaskTriggerType
  cron_expression?: string
  interval_seconds?: number
  run_at?: string
  timezone?: string
  quiet_hours_start?: string | null
  quiet_hours_end?: string | null
  misfire_policy?: TaskMisfirePolicy
  template?: string
  profile_id?: number | null
  model?: string
  tools_whitelist?: string[] | null
  capability_policy?: Record<string, string> | null
  working_directory?: string | null
  approval_policy?: TaskApprovalPolicy
  delivery_channels?: TaskDeliveryChannel[]
  delivery_config?: Record<string, unknown>
  max_iterations?: number
  max_cost_per_run?: number | null
  timeout_s?: number | null
  enabled?: boolean
}

export type ScheduledTaskUpdate = Partial<Omit<ScheduledTaskCreate, "template">>

export interface TaskRun {
  id: number
  task_id: number
  conversation_id: number | null
  run_id: number | null
  status: TaskRunStatus
  trigger_source: TaskTriggerSource
  prompt: string
  output: string | null
  error: string | null
  skip_reason: string | null
  approval_policy: TaskApprovalPolicy | null
  approval_reason: string | null
  usage: Record<string, unknown> | null
  duration_ms: number | null
  delivery_status: Record<string, string> | null
  delivered_at: string | null
  is_read: boolean
  started_at: string
  finished_at: string | null
  created_at: string
  updated_at: string
}

export interface TaskRunDetail extends TaskRun {
  messages: {
    id: number
    role: string
    content: string | null
    tool_calls: Record<string, unknown>[] | null
    tool_result: Record<string, unknown> | null
    created_at: string
  }[]
}

export interface TaskTemplate {
  slug: string
  name: string
  description: string
  prompt: string
  cron_expression: string
  tools_whitelist: string[] | null
  max_iterations: number
  delivery_channels: string[]
}

export interface ParseCronResponse {
  cron_expression: string | null
  description: string | null
  next_runs: string[]
  detail: string | null
}

export interface TaskInbox {
  unread_count: number
  runs: TaskRun[]
}

export interface SchedulerStatus {
  enabled: boolean
  running: boolean
  timezone: string
  max_concurrent_tasks: number
  jobs: { id: string; name: string; next_run_time: string | null }[]
}

// --- RSS / News Aggregator (Фаза 3b §6) ---

export interface RssSubscription {
  id: number
  user_id: number
  url: string
  title: string | null
  site_url: string | null
  category: string | null
  fetch_interval_minutes: number
  enabled: boolean
  last_fetched_at: string | null
  last_error: string | null
  entry_count: number
  created_at: string
  updated_at: string
}

export interface RssEntry {
  id: number
  subscription_id: number
  guid: string
  title: string | null
  link: string | null
  author: string | null
  summary: string | null
  published_at: string | null
  content_hash: string | null
  is_read: boolean
  fetched_at: string
}

// --- Webhook Router (Фаза 3b §7) ---

export type WebhookSourceType = "github" | "notion" | "slack" | "custom"
export type WebhookEventStatus =
  | "received"
  | "processing"
  | "completed"
  | "failed"
  | "rejected"

export interface WebhookEndpoint {
  id: number
  user_id: number
  name: string
  hook_id: string
  secret: string
  source_type: WebhookSourceType
  event_filter: string[] | null
  task_id: number | null
  prompt_template: string | null
  enabled: boolean
  created_at: string
  updated_at: string
  url_path: string
}

export interface WebhookEvent {
  id: number
  endpoint_id: number
  event_type: string | null
  payload: Record<string, unknown> | null
  signature_valid: boolean
  status: WebhookEventStatus
  task_run_id: number | null
  error: string | null
  received_at: string
  created_at: string
}

// --- Workspace / Git (Фаза 4) ---

export interface GitStatusOut {
  path: string
  is_git: boolean
  branch: string | null
  staged: string[]
  modified: string[]
  untracked: string[]
}

export interface GitLogEntryOut {
  hash: string
  message: string
  author: string
  date: string
}

export interface GitLogOut {
  path: string
  commits: GitLogEntryOut[]
}

export interface GitBranchOut {
  path: string
  branches: string[]
  current: string | null
}

export interface GitCheckoutRequest {
  path: string
  branch: string
}

// --- Deep Research (Фаза 4) ---

export interface ResearchStartRequest {
  topic: string
  depth?: number
  model?: string | null
  conversation_id?: number | null
}

export interface ResearchRerunRequest {
  model?: string | null
}

export type ResearchStatus = "queued" | "running" | "completed" | "failed" | "cancelled"

export type ResearchConfidence = "high" | "medium" | "low"

export interface ResearchSource {
  url: string
  title: string
  snippet: string
  fetched_at: string
  sub_question: string
  sub_questions: string[] | null
  confidence: ResearchConfidence | string
  conflict: boolean
}

export interface ResearchCitation {
  index: number
  text: string
  source_ids: number[]
  confidence: ResearchConfidence | string
  conflict: boolean
}

export interface ResearchUsage {
  prompt_tokens?: number
  completion_tokens?: number
  total_tokens?: number
  cost_usd?: number
}

export interface ResearchRun {
  id: number
  topic: string
  depth: number
  model: string | null
  status: ResearchStatus
  input_hash: string | null
  report_artifact_id: number | null
  sources_count: number
  citations_count: number
  usage: ResearchUsage | null
  error: string | null
  created_at: string
  updated_at: string
  finished_at: string | null
}

export interface ResearchRunDetail extends ResearchRun {
  conversation_id: number | null
  parent_task_run_id: number | null
  sub_questions: string[]
  sources: ResearchSource[]
  citations: ResearchCitation[]
  report_markdown: string | null
  browser_activity: {
    id: string
    name: string
    arguments: Record<string, unknown>
    created_at: string
    status: "running" | "completed" | "error"
    result?: string
    metadata?: Record<string, unknown>
  }[]
}

export type ResearchProgressEventType =
  | "started"
  | "stage"
  | "subquestion_started"
  | "subquestion_completed"
  | "source_found"
  | "completed"
  | "failed"
  | "cancelled"

export interface ResearchProgressEvent {
  type: ResearchProgressEventType
  payload: Record<string, unknown>
}
