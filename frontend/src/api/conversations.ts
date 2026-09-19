import { idempotencyKey, sdk } from "./sdk"
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
async function canonicalMessages(convId: number, sessionId: string): Promise<Message[]> {
  const pages: Message[][] = []
  let cursor: number | null = null
  for (let page = 0; page < MAX_HISTORY_PAGES; page += 1) {
    const result = await sdk.sessionHistory({
      sessionId,
      limit: HISTORY_PAGE_LIMIT,
      beforeCursor: cursor,
    })
    pages.push(result.items.map((item) => toMessage(item, convId)))
    if (!result.hasMore || result.nextCursor === null) break
    cursor = result.nextCursor
  }
  // Pages arrive newest-first; each page is chronological inside.
  pages.reverse()
  return pages.flat()
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
    const messages = await canonicalMessages(id, sessionId)
    return { ...toConversation(record), messages }
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

  /** Resolve a pending tool-call approval (gated behind an "ask" permission). */
  approveToolCall: async (
    _convId: number,
    approvalId: string,
    approved: boolean,
    expectedRevision: number,
    _runId: number
  ): Promise<{ resolved: boolean; approved: boolean }> => {
    await sdk.approvalResolve({
      idempotencyKey: idempotencyKey(),
      approvalId,
      expectedRevision,
      decision: approved ? "approved" : "denied",
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
