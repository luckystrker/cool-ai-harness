import { idempotencyKey, sdk } from "./sdk"
import { CoolProtocolError } from "@cool-sdk/client"
import type { JsonValue } from "./generated/cool_protocol"
import {
  toApprovalAudit,
  toCompactResponse,
  toConversation,
  toConversationCreate,
  toConversationUpdate,
  toInlineApproval,
  toMessage,
  toRun,
} from "./mappers"
import type {
  ApprovalAudit,
  Conversation,
  ConversationCreate,
  ConversationDetail,
  ConversationUpdate,
  InlineApproval,
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

/** Total `run.events` an approval restore scans per run before truncating. */
const MAX_APPROVAL_SCAN_EVENTS = 20480

/**
 * Call an RPC whose `limit` param is validated against a configured server
 * ceiling (`invalid_*_limit` errors): retry with the limit halved until the
 * server accepts it. Deployments may configure a lower ceiling than the 256
 * default and the SPA cannot read the advertised value — its pooled HTTP
 * transport connection is already initialized.
 */
async function withLimitRetry<T>(
  pageSize: { limit: number },
  call: (limit: number) => Promise<T>
): Promise<T> {
  for (let attempt = 0; attempt < 9; attempt += 1) {
    try {
      return await call(pageSize.limit)
    } catch (error) {
      const code = error instanceof CoolProtocolError ? error.protocol.coolCode : null
      if (code == null || !code.startsWith("invalid_") || !code.endsWith("_limit") || pageSize.limit <= 1) {
        throw error
      }
      pageSize.limit = Math.max(1, Math.floor(pageSize.limit / 2))
    }
  }
  throw new Error("unreachable")
}

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
    sessionId: null,
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

  /**
   * Approvals still actionable on the conversation's durable session, rebuilt
   * from the run event log: every `tool.approval_required` with no matching
   * `tool.approval_resolved` in a run parked in `awaiting_approval` is still
   * open. The history projection drops approval events, so this is what puts
   * the card back after a page reload (B4a).
   *
   * The scan is bounded by total events read (not pages — the page size can
   * shrink under `withLimitRetry`): past MAX_APPROVAL_SCAN_EVENTS a run's
   * scan is truncated and its opens are DROPPED — surfacing one could
   * resurrect a resolved approval whose `resolved` event sits beyond the
   * cap. `complete` is false when any run's scan failed or truncated; the
   * caller must then merge instead of deleting unlisted restored cards —
   * a failed scan proves nothing about their state.
   */
  pendingApprovals: async (
    convId: number
  ): Promise<{ approvals: InlineApproval[]; complete: boolean }> => {
    const sessionId = await sessionFor(convId)
    const pageSize = { limit: 256 }
    const { runs } = await withLimitRetry(pageSize, (limit) =>
      sdk.sessionRuns({ sessionId, limit })
    )
    const pending: InlineApproval[] = []
    let complete = true
    for (const run of runs) {
      if (run.status !== "awaiting_approval") continue
      const open = new Map<string, InlineApproval>()
      let afterSeq: number | null = null
      let scanned = 0
      let truncated = false
      try {
        for (;;) {
          const result = await withLimitRetry(pageSize, (limit) =>
            sdk.runEvents({ runId: run.runId, afterSeq, limit })
          )
          scanned += result.events.length
          for (const envelope of result.events) {
            const event = envelope.event
            if (event.kind === "tool.approval_required") {
              open.set(event.payload.approvalId, toInlineApproval(event.payload))
            } else if (event.kind === "tool.approval_resolved") {
              open.delete(event.payload.approvalId)
            }
          }
          if (!result.hasMore || result.nextCursor?.afterSeq == null) break
          if (scanned >= MAX_APPROVAL_SCAN_EVENTS) {
            truncated = true
            break
          }
          afterSeq = result.nextCursor.afterSeq
        }
      } catch {
        // One run's scan failing (closed run, transport error) must not
        // kill the restore for every other run — but the failed run proves
        // nothing about its approvals, so the caller merges, not deletes.
        complete = false
        continue
      }
      if (truncated) {
        complete = false
        continue
      }
      pending.push(...open.values())
    }
    return { approvals: pending, complete }
  },

  /**
   * Fork the conversation's durable session at a history cursor (P2.13):
   * copies only events up to the cursor into a new session. The server clones
   * the source conversation — model, permissions, capability policy — and
   * binds the forked session to the clone. Returns the new conversation for
   * navigation.
   */
  forkFromCursor: async (convId: number, cursor: number): Promise<Conversation> => {
    const [sessionId, source] = await Promise.all([
      sessionFor(convId),
      sdk.conversationsGet({ id: convId }),
    ])
    const forked = await sdk.sessionFork({
      idempotencyKey: idempotencyKey(),
      sessionId,
      title: null,
      upToCursor: cursor,
      upToEventSeq: null,
    })
    if (forked.conversationId != null) {
      return toConversation(await sdk.conversationsGet({ id: forked.conversationId }))
    }
    // Fallback for deployments where the fork couldn't bind a conversation
    // (no legacy store / unbound source session): create + bind client-side,
    // still carrying the parent's posture fields over.
    const conversation = await sdk.conversationsCreate(
      toConversationCreate(
        {
          title: source.title ?? undefined,
          provider: source.provider ?? undefined,
          working_directory: source.workingDirectory ?? undefined,
          model: source.model ?? undefined,
          permissions:
            (source.permissions as unknown as Conversation["permissions"] | undefined) ?? undefined,
          capability_policy:
            (source.capabilityPolicy as unknown as Conversation["capability_policy"] | undefined) ??
            undefined,
          profile_id: source.profileId ?? undefined,
          tags: (source.tags as string[] | null) ?? undefined,
          folder: source.folder ?? undefined,
          metadata: (source.metadata as Record<string, JsonValue> | null) ?? undefined,
        },
        idempotencyKey()
      )
    )
    await sdk.sessionForConversation({
      idempotencyKey: idempotencyKey(),
      conversationId: conversation.id,
      sessionId: forked.sessionId,
    })
    return toConversation(conversation)
  },

  /**
   * Rewind the conversation's durable session to a history cursor (P2.13):
   * all runs are marked `rewound` and a new run carries the history prefix.
   * `restoreWorkspace` rolls the working tree back to the newest recorded
   * filesystem checkpoint at or before the cursor (P2.18).
   */
  rewindToCursor: async (convId: number, cursor: number) => {
    const sessionId = await sessionFor(convId)
    return sdk.sessionRewind({
      idempotencyKey: idempotencyKey(),
      sessionId,
      toCursor: cursor,
      reason: "rewind from chat",
      restoreWorkspace: true,
    })
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
