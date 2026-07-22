import { useCallback, useRef, useState } from "react"
import { conversationsApi } from "@/api/conversations"
import { streamConversationMessage } from "@/api/streaming"
import type { AgentEvent, ToolApprovalRequestPayload, UsagePayload } from "@/api/types"
import type { ToolCallBlockProps } from "@/components/chat/ToolCallBlock"
import type { MessageViewModel } from "@/components/chat/MessageBubble"

/** A tool call awaiting the user's approve/deny decision. */
export interface PendingApproval {
  conversationId: number
  callId: string
  name: string
  arguments: Record<string, unknown>
  reason: string
}

interface Accumulator {
  /** Pending user message (sent but not yet persisted). */
  user?: MessageViewModel
  /** In-flight assistant message being built up from events. */
  assistant?: MessageViewModel
  /** tool_call_id → tool-call block props, kept in insertion order. */
  toolCalls: Map<string, ToolCallBlockProps & { key: string }>
  content: string
  /** Accumulated reasoning / chain-of-thought text. */
  thinking: string
  /** Usage reported by the terminal `finish` event, if any. */
  usage?: UsagePayload
  /** Reason from the terminal `finish` event, if any. */
  finishReason?: string
}

const newAcc = (): Accumulator => ({
  toolCalls: new Map(),
  content: "",
  thinking: "",
})

/**
 * Drives a single agent turn over the SSE stream and produces the two
 * optimistic messages (user + in-flight assistant) that the UI renders
 * while waiting for the persisted history to reload.
 */
export function useConversationStream() {
  const [pendingMsgs, setPendingMsgs] = useState<MessageViewModel[]>([])
  const [isStreaming, setIsStreaming] = useState(false)
  /** When set, a tool call is blocked waiting for the user's decision. */
  const [pendingApproval, setPendingApproval] = useState<PendingApproval | null>(null)
  /** Conversation id for the active stream, so respondApproval knows the URL. */
  const convIdRef = useRef<number | null>(null)
  const abortRef = useRef<AbortController | null>(null)
  /** monotonic timestamp captured when the run starts (for elapsed time). */
  const startedAtRef = useRef<number | null>(null)

  const flush = (acc: Accumulator, streaming = true) => {
    const tcs = Array.from(acc.toolCalls.values())
    const elapsedMs =
      startedAtRef.current != null
        ? Math.max(0, Math.round(performance.now() - startedAtRef.current))
        : undefined
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
    }
    const msgs = acc.user ? [acc.user, assistant] : [assistant]
    setPendingMsgs(msgs)
  }

  const applyEvent = (ev: AgentEvent, acc: Accumulator) => {
    switch (ev.kind) {
      case "thinking":
        acc.thinking += (ev.payload.text as string) || ""
        flush(acc)
        break
      case "token":
        acc.content += (ev.payload.text as string) || ""
        flush(acc)
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
        flush(acc)
        break
      }
      case "tool_approval_request": {
        const p = ev.payload as unknown as ToolApprovalRequestPayload
        const id = p.id || `tc-${acc.toolCalls.size}`
        // Ensure there's a toolCall block to mark as awaiting approval; if the
        // tool_call_start event already created it, just flip the flag.
        const existing = acc.toolCalls.get(id)
        if (existing) {
          existing.awaitingApproval = true
        } else {
          acc.toolCalls.set(id, {
            key: id,
            call: { id, name: p.name, arguments: p.arguments },
            pending: true,
            awaitingApproval: true,
          })
        }
        flush(acc)
        if (convIdRef.current != null) {
          setPendingApproval({
            conversationId: convIdRef.current,
            callId: id,
            name: p.name,
            arguments: p.arguments,
            reason: p.reason,
          })
        }
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
                call: { id, name: tc.name, arguments: tc.arguments },
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
      // start / error / tool_call_delta handled by surrounding loop.
    }
  }

  const stream = useCallback(
    async (conversationId: number, content: string, model?: string) => {
      setIsStreaming(true)
      const controller = new AbortController()
      abortRef.current = controller
      convIdRef.current = conversationId
      startedAtRef.current = performance.now()
      setPendingApproval(null)

      const acc = newAcc()
      acc.user = {
        id: `local-user-${Date.now()}`,
        role: "user",
        content,
      }
      flush(acc)

      try {
        for await (const ev of streamConversationMessage(
          conversationId,
          { content, ...(model ? { model } : {}) },
          controller.signal
        )) {
          applyEvent(ev, acc)
        }
      } catch (e) {
        if ((e as Error).name !== "AbortError") {
          acc.content += `\n\n⚠️ Stream error: ${String(e)}`
          flush(acc)
        }
      } finally {
        // Mark the assistant message as not streaming anymore (caret off),
        // freezing the final elapsed time.
        const elapsedMs =
          startedAtRef.current != null
            ? Math.max(0, Math.round(performance.now() - startedAtRef.current))
            : undefined
        startedAtRef.current = null
        setPendingMsgs((cur) =>
          cur.map((m) =>
            m.role === "assistant"
              ? { ...m, streaming: false, elapsedMs: m.elapsedMs ?? elapsedMs }
              : m
          )
        )
        setIsStreaming(false)
        abortRef.current = null
        convIdRef.current = null
        setPendingApproval(null)
      }
    },
    []
  )

  /** Resolve a pending approval via the approval REST endpoint. */
  const respondApproval = useCallback(async (approved: boolean) => {
    const pending = pendingApproval
    setPendingApproval(null)
    if (!pending) return
    try {
      await conversationsApi.approveToolCall(
        pending.conversationId,
        pending.callId,
        approved
      )
    } catch {
      // If the resolve fails (e.g. 404 — already timed out), the server-side
      // timeout/auto-deny handles the loop. Surface nothing here; the result
      // event will arrive over SSE regardless.
    }
  }, [pendingApproval])

  const cancel = useCallback(() => {
    abortRef.current?.abort()
  }, [])

  const clearPending = useCallback(() => setPendingMsgs([]), [])

  return {
    pendingMsgs,
    isStreaming,
    stream,
    cancel,
    clearPending,
    pendingApproval,
    respondApproval,
  }
}
