import { useCallback, useRef, useState } from "react"
import { streamConversationMessage } from "@/api/streaming"
import type { AgentEvent } from "@/api/types"
import type { ToolCallBlockProps } from "@/components/chat/ToolCallBlock"
import type { MessageViewModel } from "@/components/chat/MessageBubble"

interface Accumulator {
  /** Pending user message (sent but not yet persisted). */
  user?: MessageViewModel
  /** In-flight assistant message being built up from events. */
  assistant?: MessageViewModel
  /** tool_call_id → tool-call block props, kept in insertion order. */
  toolCalls: Map<string, ToolCallBlockProps & { key: string }>
  content: string
}

const newAcc = (): Accumulator => ({ toolCalls: new Map(), content: "" })

/**
 * Drives a single agent turn over the SSE stream and produces the two
 * optimistic messages (user + in-flight assistant) that the UI renders
 * while waiting for the persisted history to reload.
 */
export function useConversationStream() {
  const [pendingMsgs, setPendingMsgs] = useState<MessageViewModel[]>([])
  const [isStreaming, setIsStreaming] = useState(false)
  const abortRef = useRef<AbortController | null>(null)

  const flush = (acc: Accumulator) => {
    const tcs = Array.from(acc.toolCalls.values())
    const assistant: MessageViewModel = {
      id: "stream-assistant",
      role: "assistant",
      content: acc.content,
      streaming: true,
      toolCalls: tcs.length ? tcs : undefined,
    }
    const msgs = acc.user ? [acc.user, assistant] : [assistant]
    setPendingMsgs(msgs)
  }

  const applyEvent = (ev: AgentEvent, acc: Accumulator) => {
    switch (ev.kind) {
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
          entry.result = ev.payload.result as ToolCallBlockProps["result"]
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
                call: { id, name: tc.name, arguments: tc.arguments },
                pending: true,
              })
            }
          }
          flush(acc)
        }
        break
      }
      // start / finish / error / tool_call_delta handled by surrounding loop.
    }
  }

  const stream = useCallback(async (conversationId: number, content: string) => {
    setIsStreaming(true)
    const controller = new AbortController()
    abortRef.current = controller

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
        { content },
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
      // Mark the assistant message as not streaming anymore (caret off).
      setPendingMsgs((cur) =>
        cur.map((m) =>
          m.role === "assistant" ? { ...m, streaming: false } : m
        )
      )
      setIsStreaming(false)
      abortRef.current = null
    }
  }, [])

  const cancel = useCallback(() => {
    abortRef.current?.abort()
  }, [])

  const clearPending = useCallback(() => setPendingMsgs([]), [])

  return { pendingMsgs, isStreaming, stream, cancel, clearPending }
}
