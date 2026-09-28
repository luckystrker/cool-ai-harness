import { idempotencyKey, sdk } from "./sdk"
import type { JsonValue } from "./generated/cool_protocol"
import {
  toApprovalAudit,
  toCompactResponse,
  toConversation,
  toConversationCreate,
  toConversationUpdate,
  toMessage,
  toRun,
} from "./mappers"
import type {
  ApprovalAudit,
  Conversation,
  ConversationCreate,
  ConversationDetail,
  ConversationUpdate,
  Message,
  RunOut,
} from "./types"

export interface CompactResponse {
  status: string
  reason?: string
  message_count?: number
  messages_compacted?: number
  messages_kept?: number
  summary_length?: number
}

/** Bounded page size for the canonical transcript read. */
const HISTORY_PAGE_LIMIT = 100
const MAX_HISTORY_PAGES = 100

/**
 * Find-or-create the durable Rust session bound to a legacy conversation.
 *
 * The idempotency key is stable per conversation so repeated reads (page open,
 * the 1 Hz run poll during a turn) replay the durable link record instead of
 * re-projecting up to 10 000 legacy messages each time.
 */
async function sessionFor(convId: number): Promise<string> {
  const link = await sdk.sessionForConversation({
    idempotencyKey: `session-for-conversation-${convId}`,
    conversationId: convId,
  })
  return link.sessionId
}

/**
 * Read the whole canonical transcript newest-last.
 *
 * Rust never writes the legacy `messages` table, so after the cutover the
 * canonical session log is the transcript source of truth. Older history is
 * still projected into it once by `session.for_conversation`.
 */
async function canonicalMessages(
  convId: number,
  sessionId: string
): Promise<{ messages: Message[]; compactSummary: string | null; compactCutoff: number | null }> {
  const pages: Message[][] = []
  let cursor: number | null = null
  let summaryCursor: number | null = null
  let compactSummary: string | null = null
  let compactCutoff: number | null = null
  for (let page = 0; page < MAX_HISTORY_PAGES; page += 1) {
    const result = await sdk.sessionHistory({
      sessionId,
      limit: HISTORY_PAGE_LIMIT,
      beforeCursor: cursor,
    })
    const visible: Message[] = []
    for (const item of result.items) {
      // A summary item is the canonical rolling-summary projection, not a
      // chat message; the newest one wins.
      if (item.role === "summary") {
        if (summaryCursor === null || item.cursor > summaryCursor) {
          summaryCursor = item.cursor
          compactSummary = item.content
          compactCutoff = item.compactUpToCursor ?? null
        }
        continue
      }
      visible.push(toMessage(item, convId))
    }
    pages.push(visible)
    if (!result.hasMore || result.nextCursor === null) break
    cursor = result.nextCursor
  }
  // Pages arrive newest-first; each page is chronological inside.
  pages.reverse()
  return { messages: pages.flat(), compactSummary, compactCutoff }
}

export const conversationsApi = {
  list: async (): Promise<Conversation[]> => {
    const records = await sdk.conversationsList({
      includeMachineOwned: false,
      archived: null,
      pinned: null,
      folder: null,
      search: null,
      limit: 200,
      offset: 0,
    })
    return records.map(toConversation)
  },

  create: async (body: ConversationCreate = {}): Promise<Conversation> =>
    toConversation(await sdk.conversationsCreate(toConversationCreate(body, idempotencyKey()))),

  get: async (id: number): Promise<ConversationDetail> => {
    const [record, sessionId] = await Promise.all([
      sdk.conversationsGet({ id }),
      sessionFor(id),
    ])
    const { messages, compactSummary, compactCutoff } = await canonicalMessages(id, sessionId)
    return {
      ...toConversation(record),
      messages,
      compact_summary: compactSummary,
      compact_up_to_message_id: compactCutoff,
    }
  },

  update: async (id: number, body: ConversationUpdate): Promise<Conversation> =>
    toConversation(
      await sdk.conversationsUpdate(toConversationUpdate(id, body, idempotencyKey()))
    ),

  delete: async (id: number): Promise<{ deleted: number }> =>
    sdk.conversationsDelete({ idempotencyKey: idempotencyKey(), id }),

  /** Compact the conversation context by summarizing older messages. */
  compact: async (convId: number): Promise<CompactResponse> =>
    toCompactResponse(await sdk.conversationsCompact({ idempotencyKey: idempotencyKey(), id: convId })),

  /** Resolve a pending tool-call approval (gated behind an "ask" permission).
   * `remember` persists a policy rule for the approved call (P1.6).
   * `answer` is the question-card payload for `breakpointType "question"` (P1.8). */
  approveToolCall: async (
    _convId: number,
    approvalId: string,
    approved: boolean,
    expectedRevision: number,
    _runId: number,
    remember?: "session" | "project" | "user",
    answer?: JsonValue
  ): Promise<{ resolved: boolean; approved: boolean }> => {
    await sdk.approvalResolve({
      idempotencyKey: idempotencyKey(),
      approvalId,
      expectedRevision,
      decision: approved ? "approved" : "denied",
      remember: remember ?? null,
      rule: null,
      answer: answer ?? null,
    })
    return { resolved: true, approved }
  },

  /** List approval audit records for a conversation. */
  listApprovals: async (
    convId: number,
    params?: { run_id?: number; limit?: number }
  ): Promise<ApprovalAudit[]> => {
    const records = await sdk.conversationsApprovals({
      id: convId,
      runId: params?.run_id ?? null,
      limit: params?.limit ?? 200,
    })
    return records.map(toApprovalAudit)
  },

  /** Durable runs for the conversation's canonical session (newest first). */
  listRuns: async (convId: number, limit = 50): Promise<RunOut[]> => {
    const sessionId = await sessionFor(convId)
    const result = await sdk.sessionRuns({ sessionId, limit })
    // The one-time transcript import is an internal run, not a chat turn.
    return result.runs
      .filter((run) => run.finishReason !== "import")
      .map((run, index) => toRun(run, convId, index))
  },
}
