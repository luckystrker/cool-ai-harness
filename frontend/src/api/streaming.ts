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

        // SSE events are separated by a blank line. Servers may emit either
        // CRLF ("\r\n\r\n", e.g. sse-starlette) or LF ("\n\n") line endings —
        // accept both, preferring CRLF so we don't miss events and have them
        // pile up in the buffer until the stream closes (which made the
        // approval dialog never appear).
        let sepIndex: number
        while ((sepIndex = findFrameEnd(buffer)) !== -1) {
          const sepLen = buffer.startsWith("\r\n\r\n", sepIndex) ? 4 : 2
          const rawEvent = buffer.slice(0, sepIndex)
          buffer = buffer.slice(sepIndex + sepLen)
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

/**
 * Index of the next SSE frame boundary in `buffer`, or -1 if none yet.
 * Handles both CRLF ("\r\n\r\n") and LF ("\n\n") frame separators. A bare LF
 * boundary is also detected even when the server uses CRLF line endings, by
 * looking for two consecutive newlines regardless of carriage returns.
 */
function findFrameEnd(buffer: string): number {
  const crlf = buffer.indexOf("\r\n\r\n")
  const lf = buffer.indexOf("\n\n")
  if (crlf === -1) return lf
  if (lf === -1) return crlf
  return Math.min(crlf, lf)
}

/** Parse one SSE frame ("event: <kind>\ndata: <json>") into an AgentEvent. */
function parseSseEvent(raw: string): AgentEvent | null {
  let kind = "message"
  const dataLines: string[] = []

  for (const line of raw.split("\n")) {
    // Strip a trailing CR so CRLF-delimited frames parse identically to LF.
    const trimmed = line.endsWith("\r") ? line.slice(0, -1) : line
    if (trimmed.startsWith("event:")) {
      kind = trimmed.slice(6).trim()
    } else if (trimmed.startsWith("data:")) {
      dataLines.push(trimmed.slice(5).trim())
    }
  }

  if (dataLines.length === 0) return null

  try {
    const parsed = JSON.parse(dataLines.join("\n"))
    // The backend serializes events as {"kind": ..., "payload": {...}}
    // (AgentEvent.to_dict_json). Unwrap the inner payload so consumers can
    // access fields (text, id, arguments, …) directly on ev.payload. Fall
    // back to the raw object for flat payloads.
    const payload =
      parsed && typeof parsed === "object" && "payload" in parsed
        ? parsed.payload ?? {}
        : parsed
    const eventKind =
      parsed && typeof parsed === "object" && "kind" in parsed && parsed.kind
        ? (parsed.kind as string)
        : kind
    return { kind: eventKind as AgentEvent["kind"], payload }
  } catch {
    return null
  }
}
