import type { AgentEvent, SendMessageRequest } from "./types"

/**
 * Stream agent events for one conversation turn.
 *
 * Backend endpoint (POST /api/conversations/{id}/messages) responds with an
 * SSE stream. Because it's a POST, we can't use EventSource (GET-only) — we
 * read the response body as a ReadableStream and parse SSE frames manually.
 *
 * Each yielded AgentEvent corresponds to one server-sent event. The function
 * resolves when the stream closes (after a `finish` or `error` event).
 */
export async function* streamConversationMessage(
  conversationId: number,
  body: SendMessageRequest,
  signal?: AbortSignal
): AsyncGenerator<AgentEvent> {
  const resp = await fetch(`/api/conversations/${conversationId}/messages`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "text/event-stream" },
    body: JSON.stringify(body),
    signal,
  })

  if (!resp.ok || !resp.body) {
    let detail: unknown
    try {
      detail = await resp.json()
    } catch {
      detail = await resp.text().catch(() => undefined)
    }
    throw new Error(`Stream failed (${resp.status}): ${JSON.stringify(detail)}`)
  }

  const reader = resp.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""

  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })

      // SSE events are separated by a blank line. Parse complete ones.
      let sepIndex: number
      while ((sepIndex = buffer.indexOf("\n\n")) !== -1) {
        const rawEvent = buffer.slice(0, sepIndex)
        buffer = buffer.slice(sepIndex + 2)
        const parsed = parseSseEvent(rawEvent)
        if (parsed) yield parsed
      }
    }
    // Flush any trailing event.
    if (buffer.trim()) {
      const parsed = parseSseEvent(buffer)
      if (parsed) yield parsed
    }
  } finally {
    reader.releaseLock()
  }
}

/** Parse one SSE frame ("event: <kind>\ndata: <json>") into an AgentEvent. */
function parseSseEvent(raw: string): AgentEvent | null {
  let kind = "message"
  const dataLines: string[] = []

  for (const line of raw.split("\n")) {
    if (line.startsWith("event:")) {
      kind = line.slice(6).trim()
    } else if (line.startsWith("data:")) {
      dataLines.push(line.slice(5).trim())
    }
  }

  if (dataLines.length === 0) return null

  try {
    const payload = JSON.parse(dataLines.join("\n"))
    return { kind: kind as AgentEvent["kind"], payload }
  } catch {
    return null
  }
}
