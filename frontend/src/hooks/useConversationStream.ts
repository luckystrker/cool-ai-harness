import { useCallback, useRef, useState } from "react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import { streamConversationMessage } from "@/api/streaming"
import type {
  AgentEvent,
  Plan,
  PlanGeneratedPayload,
  PlanProgressPayload,
  PlanStepEventPayload,
  ToolApprovalRequestPayload,
  UsagePayload,
} from "@/api/types"
import type { ToolCallBlockProps } from "@/components/chat/ToolCallBlock"
import type {
  AssistantStreamBlock,
  MessageViewModel,
} from "@/components/chat/MessageBubble"
import type { InlineApproval } from "@/components/chat/ApprovalCard"

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
  /** True once the model streamed reasoning deltas this run. Used to skip
      redundant react_thought events (they repeat the same reasoning text). */
  thinkingStreamed: boolean
  /** Usage reported by the terminal `finish` event, if any. */
  usage?: UsagePayload
  /** Reason from the terminal `finish` event, if any. */
  finishReason?: string
  /** Inline approval request currently shown in the chat flow. */
  approval?: InlineApproval
  /** Model id for the current turn (shown on the live assistant message). */
  model?: string
  /** Set when the backend emitted an `error` event (turn failed). */
  errored?: boolean
  /** Plan generated during this turn (Фаза 2 §1 Planning Mode). */
  plan?: Plan
}

const newAcc = (): Accumulator => ({
  toolCalls: new Map(),
  content: "",
  thinking: "",
  blocks: [],
  thinkingStreamed: false,
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

/** Append a discrete ReAct thought as its own (separated) thinking block. */
function pushThoughtBlock(acc: Accumulator, text: string) {
  acc.blocks.push({ type: "thinking", text })
  acc.thinking += (acc.thinking ? "\n\n" : "") + text
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

/**
 * Drives a single agent turn over the SSE stream and produces the two
 * optimistic messages (user + in-flight assistant) that the UI renders
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
  const abortRef = useRef<AbortController | null>(null)
  /** monotonic timestamp captured when the run starts (for elapsed time). */
  const startedAtRef = useRef<number | null>(null)
  /** Live accumulator ref so respondApproval can mutate approval status. */
  const accRef = useRef<Accumulator | null>(null)
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
    setPendingMsgs(msgs)
  }

  const applyEvent = (ev: AgentEvent, acc: Accumulator) => {
    switch (ev.kind) {
      case "thinking": {
        const text = (ev.payload.text as string) || ""
        if (text) {
          acc.thinkingStreamed = true
          pushThinkingDelta(acc, text)
          flush(acc)
        }
        break
      }
      case "token": {
        const text = (ev.payload.text as string) || ""
        acc.content += text
        pushTextDelta(acc, text)
        flush(acc)
        break
      }
      case "react_thought": {
        // Route ReAct thoughts into the reasoning blocks so chain-of-thought
        // stays visible without the structured trace timeline. For reasoning
        // models the same text was already streamed via `thinking` events, so
        // skip it there to avoid duplicating the block content.
        const text = (ev.payload.text as string) || ""
        if (text && !acc.thinkingStreamed) {
          pushThoughtBlock(acc, text)
          flush(acc)
        }
        break
      }
      case "react_action":
      case "react_observation":
        // Tool execution is surfaced via tool_call_start / tool_result blocks;
        // the structured ReAct timeline is no longer rendered.
        break
      case "tool_call_start": {
        const id = (ev.payload.id as string) || `tc-${acc.toolCalls.size}`
        const name = (ev.payload.name as string) || "unknown"
        const args = (ev.payload.arguments as Record<string, unknown>) || {}
        acc.toolCalls.set(id, {
          key: id,
          call: { id, name, arguments: args },
          pending: true,
        })
        pushToolCall(acc, id)
        flush(acc)
        break
      }
      case "tool_result": {
        const id = (ev.payload.id as string) || ""
        const entry = acc.toolCalls.get(id)
        if (entry) {
          entry.pending = false
          entry.awaitingApproval = false
          entry.result = ev.payload.result as ToolCallBlockProps["result"]
        }
        // If this tool had an unresolved inline approval, the server resolved
        // it (timeout auto-deny) — reflect the outcome on the card.
        if (acc.approval && acc.approval.callId === id && acc.approval.status === "pending") {
          acc.approval = { ...acc.approval, status: "timed_out" }
        }
        flush(acc)
        break
      }
      case "tool_approval_request": {
        const p = ev.payload as unknown as ToolApprovalRequestPayload
        const id = p.id || `tc-${acc.toolCalls.size}`
        // arguments can be missing/null on malformed events; coerce to {}
        // so renderers (Object.keys / JSON.stringify) never crash.
        const args = p.arguments ?? {}
        // Ensure there's a toolCall block to mark as awaiting approval; if the
        // tool_call_start event already created it, just flip the flag.
        const existing = acc.toolCalls.get(id)
        if (existing) {
          existing.awaitingApproval = true
        } else {
          acc.toolCalls.set(id, {
            key: id,
            call: { id, name: p.name, arguments: args },
            pending: true,
            awaitingApproval: true,
          })
          pushToolCall(acc, id)
        }
        // Inline approval: attach the request to the assistant message so the
        // card renders directly in the chat flow (no modal popup).
        acc.approval = {
          callId: id,
          name: p.name,
          arguments: args,
          reason: p.reason,
          isBreakpoint: p.is_breakpoint,
          breakpointType: p.breakpoint_type,
          resultPreview: p.result_preview,
          currentContent: p.current_content,
          status: "pending",
        }
        flush(acc)
        break
      }
      case "message": {
        const tcs = ev.payload.tool_calls as
          | { id?: string | null; name: string; arguments: Record<string, unknown> }[]
          | undefined
        if (tcs) {
          for (const tc of tcs) {
            const id = tc.id || `tc-${tc.name}`
            if (!acc.toolCalls.has(id)) {
              acc.toolCalls.set(id, {
                key: id,
                call: {
                  id,
                  name: tc.name,
                  // arguments may be missing/null if the provider emitted a
                  // tool call without arguments; coerce to {} so the renderer
                  // (Object.keys, JSON.stringify) never crashes on undefined.
                  arguments: tc.arguments ?? {},
                },
                pending: true,
              })
            }
          }
          flush(acc)
        }
        break
      }
      case "finish": {
        acc.finishReason = (ev.payload.reason as string) || undefined
        acc.usage = ev.payload.usage as UsagePayload | undefined
        flush(acc)
        break
      }
      case "budget_alert": {
        // Surface spend crossing the alert threshold (Фаза 1.5 §5). The
        // BudgetIndicator in the header also reflects the status; this toast
        // gives immediate, in-conversation feedback.
        const window = (ev.payload.window as string) || "budget"
        const pct = Math.round((ev.payload.pct as number) || 0)
        toast.warning(`Cost budget alert (${window})`, {
          description: `Spending has reached ${pct}% of the ${window} limit.`,
        })
        break
      }
      // --- Planning Mode events (Фаза 2 §1) ---
      case "plan_generated": {
        const p = ev.payload as unknown as PlanGeneratedPayload
        acc.plan = {
          id: p.plan_id,
          conversation_id: convIdRef.current ?? 0,
          run_id: null,
          title: p.title,
          status: "draft",
          steps: p.steps,
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        }
        flush(acc)
        break
      }
      case "plan_step_start": {
        const p = ev.payload as unknown as PlanStepEventPayload
        if (acc.plan) {
          const step = acc.plan.steps.find((s) => s.position === p.position)
          if (step) step.status = "running"
          acc.plan = { ...acc.plan, status: "executing" }
          flush(acc)
        }
        break
      }
      case "plan_step_complete": {
        const p = ev.payload as unknown as PlanStepEventPayload
        if (acc.plan) {
          const step = acc.plan.steps.find((s) => s.position === p.position)
          if (step) {
            step.status = p.status ?? "completed"
            step.result_summary = p.result_summary ?? null
          }
          flush(acc)
        }
        break
      }
      case "plan_progress": {
        const p = ev.payload as unknown as PlanProgressPayload
        if (acc.plan && p.completed === p.total) {
          // All steps done — mark plan completed (or failed if any step failed).
          const hasFailed = acc.plan.steps.some((s) => s.status === "failed")
          acc.plan = { ...acc.plan, status: hasFailed ? "failed" : "completed" }
          flush(acc)
        }
        break
      }
      // --- Subagent events (Фаза 2 §5) ---
      case "subagent_started": {
        const name = (ev.payload.name as string) || "subagent"
        const role = (ev.payload.role as string) || ""
        acc.content += `\n\n> 🤖 **Subagent launched:** ${name}${role ? ` (${role})` : ""}\n`
        flush(acc)
        break
      }
      case "subagent_completed": {
        const summary = (ev.payload.result_summary as string) || "Done"
        acc.content += `> ✅ **Subagent completed:** ${summary.slice(0, 200)}\n`
        flush(acc)
        break
      }
      case "subagent_failed": {
        const error = (ev.payload.error as string) || "Unknown error"
        acc.content += `> ❌ **Subagent failed:** ${error}\n`
        flush(acc)
        break
      }
      case "subagent_progress":
        // Progress updates are too frequent to render inline; skip.
        break
      case "error": {
        // Provider / loop failures (e.g. 401 from the LLM backend). Without
        // this the stream just ends and the user sees their message with no
        // reply and no explanation.
        const message = (ev.payload.message as string) || "Unknown error"
        const detail = (ev.payload.detail as string) || ""
        acc.content += `\n\n⚠️ **Error:** ${message}${detail ? ` — ${detail}` : ""}`
        acc.finishReason = acc.finishReason ?? "error"
        acc.errored = true
        toast.error(message, { description: detail || undefined })
        flush(acc)
        break
      }
      // start / tool_call_delta handled by surrounding loop.
    }
  }

  const stream = useCallback(
    async (
      conversationId: number,
      content: string,
      model?: string,
      planMode?: boolean,
      systemPrompt?: string,
      artifactIds?: number[]
    ) => {
      setIsStreaming(true)
      const controller = new AbortController()
      abortRef.current = controller
      convIdRef.current = conversationId
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
        for await (const ev of streamConversationMessage(
          conversationId,
          {
            content,
            ...(model ? { model } : {}),
            ...(planMode ? { plan_mode: true } : {}),
            ...(systemPrompt ? { system_prompt: systemPrompt } : {}),
            ...(artifactIds?.length ? { artifact_ids: artifactIds } : {}),
          },
          controller.signal
        )) {
          applyEvent(ev, acc)
        }
      } catch (e) {
        if ((e as Error).name !== "AbortError") {
          acc.content += `\n\n⚠️ Stream error: ${String(e)}`
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
        const errored = Boolean(acc.errored)
        setPendingMsgs((cur) =>
          cur
            // On a failed turn the user message is already persisted (the
            // backend saves it before the run starts) and the refetched
            // history shows it — drop the optimistic copy to avoid a
            // duplicate, keeping only the assistant error bubble.
            .filter((m) => (errored ? m.role === "assistant" : true))
            .map((m) =>
              m.role === "assistant"
                ? { ...m, streaming: false, elapsedMs: m.elapsedMs ?? elapsedMs }
                : m
            )
        )
        setIsStreaming(false)
        abortRef.current = null
        convIdRef.current = null
        accRef.current = null
      }
      return Boolean(acc.errored)
    },
    []
  )

  /**
   * Resolve the inline approval shown in the chat flow.
   * Updates the card status (resolving → approved/denied) and calls the
   * approval REST endpoint; the agent loop resumes server-side.
   */
  const respondApproval = useCallback(async (approved: boolean) => {
    const acc = accRef.current
    const pending = acc?.approval
    if (!pending || pending.status !== "pending") return

    const resolvedCallId = pending.callId

    // Optimistically flip the card to "resolving".
    acc!.approval = { ...pending, status: "resolving" }
    flush(acc!)

    try {
      await conversationsApi.approveToolCall(
        convIdRef.current!,
        resolvedCallId,
        approved
      )
      // Only update if the current approval still refers to the same call.
      // A newer tool_approval_request may have arrived while we awaited the
      // API response (multiple tool calls in one batch); overwriting it would
      // hide the new approval card from the user.
      if (accRef.current?.approval?.callId === resolvedCallId) {
        accRef.current.approval = { ...pending, status: approved ? "approved" : "denied" }
      }
    } catch {
      // If the resolve fails (e.g. 404 — already timed out), the server-side
      // timeout/auto-deny handles the loop. Show denied so the card doesn't
      // stay stuck in "resolving" — but only if still current.
      if (accRef.current?.approval?.callId === resolvedCallId) {
        accRef.current.approval = { ...pending, status: "denied" }
      }
    }
    if (accRef.current) flush(accRef.current)
  }, [])

  const cancel = useCallback(() => {
    abortRef.current?.abort()
  }, [])

  const clearPending = useCallback(() => setPendingMsgs([]), [])

  return {
    pendingMsgs,
    setPendingMsgs,
    isStreaming,
    stream,
    cancel,
    clearPending,
    respondApproval,
  }
}
