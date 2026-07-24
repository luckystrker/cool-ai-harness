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
}

/** PATCH /api/conversations/{id} — only provided fields are applied. */
export interface ConversationUpdate {
  title?: string
  model?: string
  working_directory?: string
  permissions?: ToolPermissions
  capability_policy?: CapabilityPolicy
  breakpoints?: BreakpointConfig[]
}

/** One row of a stored message. Matches app/api/schemas.MessageOut. */
export interface Message {
  id: number
  conversation_id: number
  role: "system" | "user" | "assistant" | "tool"
  content: string | null
  tool_calls?: ToolCall[] | null
  usage?: Record<string, unknown> | null
  /** Reasoning / chain-of-thought (assistant messages), when the provider exposes one. */
  thinking?: string | null
  /** Structured tool result (role="tool" messages). */
  tool_result?: { tool_call_id?: string | null; name?: string | null; result?: ToolResultPayload } | null
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
}

export interface SendMessageRequest {
  content: string
  model?: string
  system_prompt?: string
  tool_names?: string[]
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
}

export interface ProviderUpdate {
  label?: string
  base_url?: string
  api_key?: string
  default_model?: string
  is_active?: boolean
  is_fallback?: boolean
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
