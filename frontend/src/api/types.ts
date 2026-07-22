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

export interface Conversation {
  id: number
  user_id: number
  title: string | null
  model: string | null
  /** Per-conversation working directory (overrides the global default). */
  working_directory: string | null
  /** Per-conversation tool permissions (override global defaults). */
  permissions: ToolPermissions | null
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
}

/** PATCH /api/conversations/{id} — only provided fields are applied. */
export interface ConversationUpdate {
  title?: string
  model?: string
  working_directory?: string
  permissions?: ToolPermissions
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
}

export interface ProviderUpdate {
  label?: string
  base_url?: string
  api_key?: string
  default_model?: string
  is_active?: boolean
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

/** Payload shape for a tool_approval_request event. */
export interface ToolApprovalRequestPayload {
  id: string
  name: string
  arguments: Record<string, unknown>
  reason: string
  requires_decision: true
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
