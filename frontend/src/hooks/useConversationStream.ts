import { useCallback, useRef, useState } from "react"
import { getErrorDescription } from "@/api/client"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import { streamConversationMessage } from "@/api/streaming"
import { idempotencyKey, sdk } from "@/api/sdk"
import { toInlineApproval } from "@/api/mappers"
import type { EventEnvelope } from "@/api/generated/cool_protocol"
import type {
  InlineApproval,
  Plan,
  PlanStep,
  PlanStepStatus,
  UsagePayload,
} from "@/api/types"
import type { ToolCallBlockProps } from "@/components/chat/ToolCallBlock"
import type {
  AssistantStreamBlock,
  MessageViewModel,
} from "@/components/chat/MessageBubble"
import type { JsonValue } from "@/api/generated/cool_protocol"

/**
 * Internal ordered block used while accumulating a live turn. Thinking blocks
 * hold raw text; tool blocks reference tool-call ids (resolved against the
 * accumulator's toolCalls map at flush time so result updates are reflected).
 */
type AccBlock =
  | { type: "thinking"; text: string }
  | { type: "tools"; ids: string[] }
  | { type: "text"; text: string }

interface Accumulator {
  /** Pending user message (sent but not yet persisted). */
  user?: MessageViewModel
  /** In-flight assistant message being built up from events. */
  assistant?: MessageViewModel
  /** tool_call_id → tool-call block props, kept in insertion order. */
  toolCalls: Map<string, ToolCallBlockProps & { key: string }>
  content: string
  /** Accumulated reasoning / chain-of-thought text (flat, for the hint). */
  thinking: string
  /** Ordered interleaved blocks (thinking/tools) for live rendering. */
  blocks: AccBlock[]
  /** Usage reported by the terminal `finish` event, if any. */
  usage?: UsagePayload
  /** Reason from the terminal `finish` event, if any. */
  finishReason?: string
  /** Inline approval request currently shown in the chat flow. */
  approval?: InlineApproval
  /** Model id for the current turn (shown on the live assistant message). */
  model?: string
  /** Set when the run emitted a failure event (turn failed). */
  errored?: boolean
  /**
   * Set when a terminal event arrived over the stream — the same marker the
   * server projects into `session.history`, so the refetched transcript
   * already carries the failure/cancel note and the pending bubble would
   * duplicate it.
   */
  persistedTerminal?: boolean
  /** Plan generated during this turn (Фаза 2 §1 Planning Mode). */
  plan?: Plan
}

const newAcc = (): Accumulator => ({
  toolCalls: new Map(),
  content: "",
  thinking: "",
  blocks: [],
})

/** Append a streamed reasoning delta to the current (or a new) thinking block. */
function pushThinkingDelta(acc: Accumulator, text: string) {
  const last = acc.blocks[acc.blocks.length - 1]
  if (last && last.type === "thinking") {
    last.text += text
  } else {
    acc.blocks.push({ type: "thinking", text })
  }
  acc.thinking += text
}

/** Add a tool-call id to the current (or a new) tools block. */
function pushToolCall(acc: Accumulator, id: string) {
  const last = acc.blocks[acc.blocks.length - 1]
  if (last && last.type === "tools") {
    last.ids.push(id)
  } else {
    acc.blocks.push({ type: "tools", ids: [id] })
  }
}

/** Append streamed text content to the current (or a new) text block. */
function pushTextDelta(acc: Accumulator, text: string) {
  const last = acc.blocks[acc.blocks.length - 1]
  if (last && last.type === "text") {
    last.text += text
  } else {
    acc.blocks.push({ type: "text", text })
  }
}

/** Coerce a canonical JSON result into the tool-result view shape. */
function toolResult(result: unknown): ToolCallBlockProps["result"] {
  if (typeof result === "string") return { output: result, is_error: false }
  if (result !== null && typeof result === "object") {
    return {
      output: JSON.stringify(result),
      is_error: false,
      metadata: result as Record<string, unknown>,
    }
  }
  return { output: result === null || result === undefined ? "(empty)" : String(result), is_error: false }
}

/**
 * Drives a single agent turn over the canonical cursor stream and produces the
 * two optimistic messages (user + in-flight assistant) that the UI renders
 * while waiting for the persisted history to reload.
 *
 * Approvals are rendered inline in the chat (no modal): the assistant
 * message carries an `approval` field with Allow/Deny buttons.
 */
export function useConversationStream() {
  const [pendingMsgs, setPendingMsgs] = useState<MessageViewModel[]>([])
  const [isStreaming, setIsStreaming] = useState(false)
  /** Conversation id for the active stream, so respondApproval knows the URL. */
  const convIdRef = useRef<number | null>(null)
  /** Durable run id of the active turn, so `cancel` can stop it server-side. */
  const runIdRef = useRef<string | null>(null)
  const abortRef = useRef<AbortController | null>(null)
  /** monotonic timestamp captured when the run starts (for elapsed time). */
  const startedAtRef = useRef<number | null>(null)
  /** Live accumulator ref so respondApproval can mutate approval status. */
  const accRef = useRef<Accumulator | null>(null)
  /**
   * Approvals restored from the server (still pending but not owned by the
   * live accumulator): approvalId -> the card's latest view model. The
   * history projection drops approval events, so a run parked in
   * `awaiting_approval` would otherwise be unresolvable after a reload (B4a).
   */
  const restoredRef = useRef(new Map<string, InlineApproval>())
  /** rAF throttle: avoids per-token React re-renders (batches to ~60fps). */
  const rafRef = useRef<number | null>(null)
  const flushScheduledRef = useRef(false)

  const flush = (acc: Accumulator, streaming = true) => {
    // Schedule a rAF-throttled render. Multiple flush() calls within one frame
    // coalesce into a single setState, reducing re-renders from O(tokens) to
    // O(frames). Non-streaming flushes (finish) are immediate for correctness.
    if (!streaming) {
      _doFlush(acc, streaming)
      return
    }
    if (flushScheduledRef.current) return // already scheduled
    flushScheduledRef.current = true
    rafRef.current = requestAnimationFrame(() => {
      flushScheduledRef.current = false
      _doFlush(acc, streaming)
    })
  }

  const _doFlush = (acc: Accumulator, streaming: boolean) => {
    const tcs = Array.from(acc.toolCalls.values())
    const elapsedMs =
      startedAtRef.current != null
        ? Math.max(0, Math.round(performance.now() - startedAtRef.current))
        : undefined
    // Resolve the ordered accumulator blocks into renderable view blocks,
    // mapping tool ids back to their (live-updating) tool-call props.
    const blocks: AssistantStreamBlock[] = acc.blocks
      .map((b) =>
        b.type === "thinking"
          ? { type: "thinking" as const, text: b.text }
          : b.type === "text"
            ? { type: "text" as const, text: b.text }
            : {
                type: "tools" as const,
                calls: b.ids
                  .map((id) => acc.toolCalls.get(id))
                  .filter((c): c is ToolCallBlockProps & { key: string } => c != null),
              }
      )
      .filter((b) =>
        b.type === "thinking" ? b.text.length > 0
        : b.type === "text" ? b.text.length > 0
        : b.calls.length > 0
      )
    const assistant: MessageViewModel = {
      id: "stream-assistant",
      role: "assistant",
      content: acc.content,
      streaming,
      thinking: acc.thinking || undefined,
      elapsedMs,
      usage: acc.usage,
      finishReason: acc.finishReason,
      toolCalls: tcs.length ? tcs : undefined,
      blocks: blocks.length ? blocks : undefined,
      approval: acc.approval,
      model: acc.model,
      createdAt: acc.user?.createdAt,
      plan: acc.plan,
    }
    const msgs = acc.user ? [acc.user, assistant] : [assistant]
    setPendingMsgs([...msgs, ...restoredMsgs()])
  }

  /**
   * Restored approvals render as tail assistant messages carrying only the
   * card. An approval owned by the live accumulator is skipped — the same
   * card must never render twice.
   */
  const restoredMsgs = (): MessageViewModel[] => {
    const liveId = accRef.current?.approval?.approvalId
    return [...restoredRef.current.entries()]
      .filter(([id]) => id !== liveId)
      .map(([id, approval]) => ({
        id: `approval-restored-${id}`,
        role: "assistant" as const,
        content: "",
        approval,
      }))
  }

  /** Apply one canonical event envelope to the live accumulator. */
  const applyCanonical = (envelope: EventEnvelope, acc: Accumulator) => {
    const canonical = envelope.event
    switch (canonical.kind) {
      case "run.started": {
        acc.model = acc.model ?? canonical.payload.model ?? undefined
        break
      }
      case "content.delta": {
        acc.content += canonical.payload.text
        pushTextDelta(acc, canonical.payload.text)
        flush(acc)
        break
      }
      case "reasoning.delta": {
        pushThinkingDelta(acc, canonical.payload.text)
        flush(acc)
        break
      }
      case "usage.updated": {
        acc.usage = {
          prompt_tokens: canonical.payload.promptTokens,
          completion_tokens: canonical.payload.completionTokens,
          total_tokens: canonical.payload.totalTokens,
          cost_usd: canonical.payload.costUsd,
        }
        flush(acc)
        break
      }
      case "tool.requested": {
        const { callId, name, arguments: args } = canonical.payload
        if (!acc.toolCalls.has(callId)) {
          acc.toolCalls.set(callId, {
            key: callId,
            call: { id: callId, name, arguments: args as Record<string, unknown> },
            pending: true,
          })
          pushToolCall(acc, callId)
        }
        flush(acc)
        break
      }
      case "tool.approval_required": {
        const p = canonical.payload
        const existing = acc.toolCalls.get(p.callId)
        if (existing) {
          existing.awaitingApproval = true
        } else {
          acc.toolCalls.set(p.callId, {
            key: p.callId,
            call: { id: p.callId, name: p.name, arguments: p.arguments as Record<string, unknown> },
            pending: true,
            awaitingApproval: true,
          })
          pushToolCall(acc, p.callId)
        }
        acc.approval = toInlineApproval(p)
        flush(acc)
        break
      }
      case "tool.approval_resolved": {
        const p = canonical.payload
        if (acc.approval?.approvalId === p.approvalId) {
          acc.approval = { ...acc.approval, status: p.decision }
        }
        const entry = acc.toolCalls.get(p.callId)
        if (entry) entry.awaitingApproval = false
        flush(acc)
        break
      }
      case "tool.started": {
        const entry = acc.toolCalls.get(canonical.payload.callId)
        if (entry) {
          entry.pending = true
          entry.awaitingApproval = false
        }
        flush(acc)
        break
      }
      case "tool.completed": {
        const p = canonical.payload
        const entry = acc.toolCalls.get(p.callId)
        if (entry) {
          entry.pending = false
          entry.awaitingApproval = false
          entry.result = toolResult(p.result)
        }
        if (acc.approval && acc.approval.callId === p.callId && acc.approval.status === "pending") {
          acc.approval = { ...acc.approval, status: "approved" }
        }
        flush(acc)
        break
      }
      case "tool.failed": {
        const p = canonical.payload
        const message = p.message ?? p.errorCode
        const entry = acc.toolCalls.get(p.callId)
        if (entry) {
          entry.pending = false
          entry.awaitingApproval = false
          entry.result = { output: message, is_error: true, error: message }
        }
        if (acc.approval && acc.approval.callId === p.callId && acc.approval.status === "pending") {
          acc.approval = { ...acc.approval, status: "denied" }
        }
        flush(acc)
        break
      }
      case "run.completed": {
        acc.finishReason = canonical.payload.reason
        flush(acc)
        break
      }
      case "run.cancelled": {
        const reason = canonical.payload.reason
        acc.finishReason = reason ?? "cancelled"
        // A client disconnect ends the stream, not the run — the durable
        // projection skips it too, so rendering a marker here would diverge.
        if (reason !== "disconnect") {
          // Same marker `session.history` appends to the run's last assistant
          // item — keep live and reloaded transcripts identical.
          const note = `\n\n🛑 **Run cancelled:** ${reason ?? "cancelled"}`
          acc.content += note
          pushTextDelta(acc, note)
          acc.persistedTerminal = true
        }
        flush(acc)
        break
      }
      case "run.failed": {
        const { reason, errorCode } = canonical.payload
        const message =
          errorCode && reason && errorCode !== reason
            ? `${errorCode}: ${reason}`
            : (errorCode ?? reason)
        // Same marker `session.history` appends to the run's last assistant
        // item — keep live and reloaded transcripts identical.
        const note = `\n\n⚠️ **Run failed:** ${message}`
        acc.content += note
        pushTextDelta(acc, note)
        acc.finishReason = acc.finishReason ?? "error"
        acc.errored = true
        acc.persistedTerminal = true
        toast.error(message)
        flush(acc)
        break
      }
      case "budget.warning":
      case "budget.exceeded": {
        const p = canonical.payload
        toast.warning(`Cost budget alert (${p.window})`, {
          description: `Spending has reached ${Math.round(p.percent)}% of the ${p.window} limit.`,
        })
        break
      }
      case "plan.created": {
        const p = canonical.payload
        acc.plan = {
          // The app server persists a durable plan when the run's session is
          // bound to a conversation and stamps its numeric id; a plan without
          // a store id cannot be approved/executed through the protocol.
          id: p.storePlanId ?? 0,
          conversation_id: convIdRef.current ?? 0,
          run_id: null,
          title: p.title,
          status: "draft",
          // `steps` is optional on the wire (older producers omit it).
          steps: (p.steps ?? []).map((step) => ({
            position: step.position,
            title: step.title,
            status: step.status as PlanStepStatus,
            result_summary: step.resultSummary,
          })),
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        }
        flush(acc)
        break
      }
      case "plan.step_started":
      case "plan.step_completed": {
        const p = canonical.payload
        const current = acc.plan ?? {
          id: 0,
          conversation_id: convIdRef.current ?? 0,
          run_id: null,
          title: null,
          status: "draft" as const,
          steps: [] as PlanStep[],
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        }
        const steps = [...current.steps]
        const index = steps.findIndex((step) => step.position === p.position)
        const next: PlanStep = {
          position: p.position,
          title: p.title,
          status: p.status as PlanStepStatus,
          result_summary: p.resultSummary,
        }
        if (index >= 0) steps[index] = next
        else steps.push(next)
        steps.sort((left, right) => left.position - right.position)
        acc.plan = {
          ...current,
          steps,
          status: canonical.kind === "plan.step_started" ? "executing" : current.status,
          updated_at: new Date().toISOString(),
        }
        flush(acc)
        break
      }
      case "plan.progress": {
        const p = canonical.payload
        if (acc.plan) {
          acc.plan = { ...acc.plan, status: p.status, updated_at: new Date().toISOString() }
          flush(acc)
        }
        break
      }
      case "subagent.started": {
        const name = canonical.payload.name ?? "subagent"
        const note = `\n\n> 🤖 **Subagent launched:** ${name}\n`
        acc.content += note
        pushTextDelta(acc, note)
        flush(acc)
        break
      }
      case "subagent.completed": {
        const summary = canonical.payload.summary ?? "Done"
        const note = `> ✅ **Subagent completed:** ${summary.slice(0, 200)}\n`
        acc.content += note
        pushTextDelta(acc, note)
        flush(acc)
        break
      }
      case "subagent.failed": {
        // The runtime reports a cancellation through the same event kind with
        // `status: "cancelled"`, so render it distinctly.
        const note =
          canonical.payload.status === "cancelled"
            ? `> 🛑 **Subagent cancelled**\n`
            : `> ❌ **Subagent failed:** ${canonical.payload.error ?? "Unknown error"}\n`
        acc.content += note
        pushTextDelta(acc, note)
        flush(acc)
        break
      }
      case "subagent.progress":
        // Progress updates are too frequent to render inline; skip.
        break
      case "session.compacted": {
        const note = `\n\n> 🗜️ **Compacted** — older turns summarized (kept ${canonical.payload.retainedItems} recent items).\n`
        acc.content += note
        pushTextDelta(acc, note)
        flush(acc)
        break
      }
      default:
        break
    }
  }

  const stream = useCallback(
    async (
      conversationId: number,
      content: string,
      model?: string,
      planMode?: boolean,
      systemPrompt?: string,
      artifactIds?: number[],
      longTaskMode?: boolean
    ) => {
      setIsStreaming(true)
      const controller = new AbortController()
      abortRef.current = controller
      convIdRef.current = conversationId
      runIdRef.current = null
      startedAtRef.current = performance.now()

      const acc = newAcc()
      accRef.current = acc
      acc.model = model
      acc.user = {
        id: `local-user-${Date.now()}`,
        role: "user",
        content,
        createdAt: new Date().toISOString(),
      }
      flush(acc)

      try {
        for await (const envelope of streamConversationMessage(
          conversationId,
          {
            content,
            ...(model ? { model } : {}),
            ...(planMode ? { plan_mode: true } : {}),
            ...(longTaskMode ? { long_task_mode: true } : {}),
            ...(systemPrompt ? { system_prompt: systemPrompt } : {}),
            ...(artifactIds?.length ? { artifact_ids: artifactIds } : {}),
          },
          controller.signal
        )) {
          runIdRef.current = envelope.runId
          applyCanonical(envelope, acc)
        }
      } catch (e) {
        if ((e as Error).name !== "AbortError") {
          const note = `\n\n**Reply interrupted.** ${getErrorDescription(
            e,
            "Check that Cool is running locally, then send your message again."
          )}`
          acc.content += note
          pushTextDelta(acc, note)
          acc.errored = true
          flush(acc)
        }
      } finally {
        // Cancel any pending rAF-throttled flush.
        if (rafRef.current != null) {
          cancelAnimationFrame(rafRef.current)
          rafRef.current = null
        }
        flushScheduledRef.current = false
        // Mark the assistant message as not streaming anymore (caret off),
        // freezing the final elapsed time.
        const elapsedMs =
          startedAtRef.current != null
            ? Math.max(0, Math.round(performance.now() - startedAtRef.current))
            : undefined
        startedAtRef.current = null
        // A persisted terminal event is re-rendered from refetched history —
        // keeping the pending bubble would draw the same marker twice. Only
        // the client-catch path (stream dropped with no server event) keeps
        // the local "Reply interrupted" bubble.
        const errored = Boolean(acc.errored) && !acc.persistedTerminal
        setPendingMsgs((cur) =>
          cur
            // On a failed turn the user message is already persisted (the
            // backend saves it before the run starts) and the refetched
            // history shows it — drop the optimistic copy to avoid a
            // duplicate, keeping only the assistant error bubble.
            .filter((m) => (errored ? m.role === "assistant" : true))
            .map((m) =>
              m.role === "assistant"
                ? {
                    ...m,
                    streaming: false,
                    elapsedMs: m.elapsedMs ?? elapsedMs,
                    // Once the stream ends nothing owns the live accumulator's
                    // unresolved card: left attached it would render a
                    // duplicate beside the restored tail card (SSE drop while
                    // parked) or a stale forever-"pending" card that errors
                    // on every click (cancel while parked). Resolved badges
                    // stay as history. Restored tail cards are owned by
                    // restoredRef and must keep their approval — strip only
                    // the accumulator's own.
                    approval:
                      m.approval != null &&
                      m.approval.approvalId === acc.approval?.approvalId &&
                      (m.approval.status === "pending" || m.approval.status === "resolving")
                        ? undefined
                        : m.approval,
                  }
                : m
            )
        )
        setIsStreaming(false)
        abortRef.current = null
        convIdRef.current = null
        accRef.current = null
        runIdRef.current = null
      }
      return Boolean(acc.errored) && !acc.persistedTerminal
    },
    []
  )

  /**
   * Bring back the actionable approval cards the server still holds open.
   * Called with `conversationsApi.pendingApprovals` results on conversation
   * load and whenever they are refetched; the map is reconciled wholesale so
   * resolved/expired approvals drop their restored card on the next result.
   */
  const restoreApprovals = useCallback((approvals: InlineApproval[], conversationId: number) => {
    // A live stream owns its own approval card — its accumulator wins.
    if (convIdRef.current !== null && convIdRef.current !== conversationId) return
    const liveId = accRef.current?.approval?.approvalId
    const next = new Map(approvals.filter((a) => a.approvalId !== liveId).map((a) => [a.approvalId, a]))
    restoredRef.current.forEach((_approval, id) => {
      if (!next.has(id)) restoredRef.current.delete(id)
    })
    next.forEach((incoming, id) => {
      // While the user is mid-click the card is locally "resolving"; the
      // refetched snapshot still reports it pending — don't flicker back.
      if (restoredRef.current.get(id)?.status !== "resolving") {
        restoredRef.current.set(id, incoming)
      }
    })
    setPendingMsgs((cur) => [
      ...cur.filter((m) => !m.id.startsWith("approval-restored-")),
      ...restoredMsgs(),
    ])
  }, [])

  /**
   * Resolve the inline approval shown in the chat flow.
   * Updates the card status (resolving → approved/denied) and calls the
   * canonical `approval.resolve` command; the agent loop resumes server-side.
   * `approvalId` selects a restored card; without it the live pending
   * approval (or the first restored one) is resolved.
   */
  const respondApproval = useCallback(async (approved: boolean, remember?: "session" | "project" | "user", answer?: JsonValue, approvalId?: string) => {
    const live = accRef.current?.approval
    const pending =
      approvalId != null && approvalId !== live?.approvalId
        ? restoredRef.current.get(approvalId)
        : (live ?? (approvalId == null
            ? [...restoredRef.current.values()].find((a) => a.status === "pending")
            : undefined))
    if (!pending || pending.status !== "pending") return

    const setStatus = (status: InlineApproval["status"]) => {
      const next = { ...pending, status }
      if (accRef.current?.approval?.approvalId === pending.approvalId) {
        accRef.current.approval = next
        flush(accRef.current)
      }
      if (restoredRef.current.has(pending.approvalId)) {
        restoredRef.current.set(pending.approvalId, next)
        setPendingMsgs((cur) =>
          cur.map((m) =>
            m.approval?.approvalId === pending.approvalId ? { ...m, approval: next } : m
          )
        )
      }
    }

    // Optimistically flip the card to "resolving".
    setStatus("resolving")

    try {
      await conversationsApi.approveToolCall(
        // Canonical resolves address the approval id; the legacy numeric
        // conversation/run ids are unused by the RPC.
        convIdRef.current ?? 0,
        pending.approvalId,
        approved,
        pending.revision,
        pending.runId,
        remember,
        answer
      )
      setStatus(approved ? "approved" : "denied")
    } catch (error) {
      // The backend never received a decision — the card must stay honest
      // and actionable so the user can retry (B4b). A failed resolve never
      // marks the card "Denied": the server still holds it pending.
      setStatus("pending")
      toast.error("Could not submit the approval decision", {
        description: getErrorDescription(error, "Check the connection and try again."),
      })
    }
  }, [])

  const cancel = useCallback(() => {
    const runId = runIdRef.current
    abortRef.current?.abort()
    // Aborting the SSE stream does not stop the run server-side, so ask the
    // runtime to cancel it (best-effort; the run may already be terminal).
    if (runId && !runId.startsWith("legacy-run-")) {
      void sdk
        .runCancel({ idempotencyKey: idempotencyKey(), runId, reason: "client_cancelled" })
        .catch(() => undefined)
    }
  }, [])

  const clearPending = useCallback(() => {
    restoredRef.current.clear()
    setPendingMsgs([])
  }, [])

  return {
    pendingMsgs,
    setPendingMsgs,
    isStreaming,
    stream,
    cancel,
    clearPending,
    respondApproval,
    restoreApprovals,
  }
}
