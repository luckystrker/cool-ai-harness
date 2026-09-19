// Record -> view-type mappers for the canonical App Protocol boundary.
//
// The generated protocol records use camelCase and JsonValue attributes; the
// hand-written `types.ts` mirrors the Python Pydantic schemas (snake_case).
// Keeping the conversion here lets `src/api/*` present the same shapes to the
// pages whether a call is served by Rust or by the Python fallback.
import type * as protocol from "./generated/cool_protocol"
import type {
  ApprovalAudit,
  CapabilityPolicy,
  Conversation,
  Message,
  RunOut,
  ToolCall,
  ToolPermissions,
  ToolResultPayload,
} from "./types"

function asObject(value: protocol.JsonValue | null | undefined): Record<string, unknown> | null {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return null
}

function asStringArray(value: protocol.JsonValue | null | undefined): string[] {
  if (!Array.isArray(value)) return []
  return value.filter((item): item is string => typeof item === "string")
}

function parsedJson(value: string | null): protocol.JsonValue | null {
  if (value === null) return null
  try {
    return JSON.parse(value) as protocol.JsonValue
  } catch {
    return value
  }
}

/** `ConversationRecord` -> `Conversation` (breakpoints live in metadata). */
export function toConversation(record: protocol.ConversationRecord): Conversation {
  const metadata = asObject(record.metadata)
  return {
    id: record.id,
    user_id: record.userId,
    title: record.title,
    model: record.model,
    working_directory: record.workingDirectory,
    permissions: (record.permissions as unknown as ToolPermissions | null) ?? null,
    capability_policy: (record.capabilityPolicy as unknown as CapabilityPolicy | null) ?? null,
    breakpoints: (metadata?.breakpoints as Conversation["breakpoints"] | undefined) ?? null,
    profile_id: record.profileId,
    tags: asStringArray(record.tags),
    folder: record.folder,
    is_pinned: record.isPinned,
    is_archived: record.isArchived,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

/** `ConversationCreate` -> canonical create params. */
export function toConversationCreate(
  body: {
    title?: string
    model?: string
    working_directory?: string
    permissions?: ToolPermissions
    capability_policy?: CapabilityPolicy
    breakpoints?: Conversation["breakpoints"]
    profile_id?: number
  },
  idempotencyKey: string
): protocol.ConversationCreateParams {
  return {
    idempotencyKey,
    title: body.title ?? null,
    provider: null,
    model: body.model ?? null,
    workingDirectory: body.working_directory ?? null,
    permissions: (body.permissions as unknown as protocol.JsonValue | undefined) ?? null,
    capabilityPolicy: (body.capability_policy as unknown as protocol.JsonValue | undefined) ?? null,
    profileId: body.profile_id ?? null,
    tags: null,
    folder: null,
    metadata: body.breakpoints ? { breakpoints: body.breakpoints as unknown as protocol.JsonValue } : null,
  }
}

/** `ConversationUpdate` -> canonical update params. */
export function toConversationUpdate(
  id: number,
  body: {
    title?: string
    model?: string
    working_directory?: string
    permissions?: ToolPermissions
    capability_policy?: CapabilityPolicy
    breakpoints?: Conversation["breakpoints"]
    profile_id?: number
    tags?: string[]
    folder?: string
    is_pinned?: boolean
    is_archived?: boolean
  },
  idempotencyKey: string
): protocol.ConversationUpdateParams {
  return {
    idempotencyKey,
    id,
    title: body.title ?? null,
    provider: null,
    model: body.model ?? null,
    workingDirectory: body.working_directory ?? null,
    permissions: (body.permissions as unknown as protocol.JsonValue | undefined) ?? null,
    capabilityPolicy: (body.capability_policy as unknown as protocol.JsonValue | undefined) ?? null,
    profileId: body.profile_id ?? null,
    tags: (body.tags as unknown as protocol.JsonValue | undefined) ?? null,
    folder: body.folder ?? null,
    isPinned: body.is_pinned ?? null,
    isArchived: body.is_archived ?? null,
    metadata: body.breakpoints ? { breakpoints: body.breakpoints as unknown as protocol.JsonValue } : null,
  }
}

function toToolResult(item: protocol.HistoryItem): Message["tool_result"] {
  if (item.role !== "tool") return null
  const parsed = parsedJson(item.content)
  let result: ToolResultPayload
  if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const object = parsed as Record<string, unknown>
    if (typeof object.errorCode === "string") {
      const message = typeof object.error === "string" ? object.error : object.errorCode
      result = { output: message, is_error: true, error: message }
    } else {
      result = { output: JSON.stringify(object), is_error: false, metadata: object }
    }
  } else {
    const output = typeof parsed === "string" ? parsed : parsed === null ? "" : JSON.stringify(parsed)
    result = { output, is_error: false }
  }
  return { tool_call_id: item.toolCallId, name: item.name, result }
}

function toUsage(item: protocol.HistoryItem): Record<string, unknown> | null {
  if (!item.usage) return null
  return {
    prompt_tokens: item.usage.promptTokens,
    completion_tokens: item.usage.completionTokens,
    total_tokens: item.usage.totalTokens,
    cost_usd: item.usage.costUsd,
  }
}

/** One canonical `HistoryItem` -> the stored-message shape the chat renders. */
export function toMessage(item: protocol.HistoryItem, conversationId: number): Message {
  const toolCalls: ToolCall[] | null = item.toolCalls.length
    ? item.toolCalls.map((call) => ({
        id: call.callId,
        name: call.name,
        arguments: call.arguments as Record<string, unknown>,
      }))
    : null
  return {
    id: item.cursor,
    conversation_id: conversationId,
    role: item.role as Message["role"],
    content: item.content,
    tool_calls: toolCalls,
    tool_result: toToolResult(item),
    usage: toUsage(item),
    thinking: item.reasoning,
    model: item.model,
    created_at: item.occurredAt,
  }
}

/** `SessionRunSummary` -> legacy `RunOut` (the recorder only reads status). */
export function toRun(
  summary: protocol.SessionRunSummary,
  conversationId: number,
  /** Position in the newest-first list; keeps the legacy id sort stable. */
  index = 0
): RunOut {
  return {
    id: -index,
    conversation_id: conversationId,
    status: summary.status,
    model: null,
    iterations: 0,
    usage: null,
    finish_reason: summary.finishReason,
    error: null,
    started_at: summary.updatedAt,
    finished_at: null,
    created_at: summary.updatedAt,
    updated_at: summary.updatedAt,
  }
}

/** `CompactResult` -> the compact response shape pages expect. */
export function toCompactResponse(result: protocol.CompactResult) {
  return {
    status: result.status,
    reason: result.reason ?? undefined,
    message_count: result.messageCount ?? undefined,
    messages_compacted: result.messagesCompacted ?? undefined,
    messages_kept: result.messagesKept ?? undefined,
    summary_length: result.summaryLength ?? undefined,
  }
}

/** `ApprovalAuditRecord` -> `ApprovalAudit` (snake_case). */
export function toApprovalAudit(record: protocol.ApprovalAuditRecord): ApprovalAudit {
  return {
    id: record.id,
    conversation_id: record.conversationId,
    run_id: record.runId,
    call_id: record.callId,
    tool_name: record.toolName,
    arguments: asObject(record.arguments),
    approved: record.approved,
    decision_source: record.decisionSource,
    decided_by: record.decidedBy,
    reason: record.reason,
    is_breakpoint: record.isBreakpoint,
    breakpoint_type: record.breakpointType,
    duration_ms: record.durationMs,
    created_at: record.createdAt,
  }
}
