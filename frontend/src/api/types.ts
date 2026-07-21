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

export interface Conversation {
  id: number
  user_id: number
  title: string | null
  model: string | null
  created_at: string
  updated_at: string
}

export interface ConversationCreate {
  title?: string
  system_prompt?: string
  model?: string
  tool_names?: string[]
}

/** One row of a stored message. Matches app/api/schemas.MessageOut. */
export interface Message {
  id: number
  conversation_id: number
  role: "system" | "user" | "assistant" | "tool"
  content: string | null
  tool_calls?: ToolCall[] | null
  usage?: Record<string, unknown> | null
  created_at: string
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
  | "token"
  | "tool_call_start"
  | "tool_call_delta"
  | "tool_result"
  | "message"
  | "finish"
  | "error"

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
