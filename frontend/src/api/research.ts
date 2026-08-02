import { api } from "./client"
import type {
  ResearchProgressEvent,
  ResearchRerunRequest,
  ResearchRun,
  ResearchRunDetail,
  ResearchStartRequest,
} from "./types"

/**
 * Deep research workflow API (Фаза 4).
 *
 * `streamResearch` drives a research run over SSE (POST /api/research/stream)
 * and yields progress events ({type, payload}) as they arrive: started,
 * stage, subquestion_started/completed, source_found, then a terminal
 * completed/failed/cancelled event.
 */

export const deepResearchApi = {
  list: (limit = 50) => api.get<ResearchRun[]>(`/api/research?limit=${limit}`),
  get: (id: number) => api.get<ResearchRunDetail>(`/api/research/${id}`),
  /** Start a background run (no live progress; poll the detail endpoint). */
  start: (body: ResearchStartRequest) => api.post<ResearchRun>("/api/research", body),
  cancel: (id: number) => api.post<{ cancelled: number }>(`/api/research/${id}/cancel`),
  rerun: (id: number, body: ResearchRerunRequest = {}) =>
    api.post<ResearchRun>(`/api/research/${id}/rerun`, body),
  exportUrl: (id: number, format: "md" | "html") =>
    `/api/research/${id}/export?format=${format}`,
}

export async function* streamResearch(
  body: ResearchStartRequest,
  signal?: AbortSignal
): AsyncGenerator<ResearchProgressEvent> {
  const resp = await fetch("/api/research/stream", {
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
    throw new Error(`Research stream failed (${resp.status}): ${JSON.stringify(detail)}`)
  }

  const reader = resp.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""

  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })

      let sepIndex: number
      while ((sepIndex = findFrameEnd(buffer)) !== -1) {
        const sepLen = buffer.startsWith("\r\n\r\n", sepIndex) ? 4 : 2
        const rawEvent = buffer.slice(0, sepIndex)
        buffer = buffer.slice(sepIndex + sepLen)
        const parsed = parseResearchEvent(rawEvent)
        if (parsed) yield parsed
      }
    }
    if (buffer.trim()) {
      const parsed = parseResearchEvent(buffer)
      if (parsed) yield parsed
    }
  } finally {
    reader.releaseLock()
  }
}

function findFrameEnd(buffer: string): number {
  const crlf = buffer.indexOf("\r\n\r\n")
  const lf = buffer.indexOf("\n\n")
  if (crlf === -1) return lf
  if (lf === -1) return crlf
  return Math.min(crlf, lf)
}

/** Parse one SSE frame ("event: research\ndata: {"type":...,"payload":...}"). */
function parseResearchEvent(raw: string): ResearchProgressEvent | null {
  const dataLines: string[] = []
  for (const line of raw.split("\n")) {
    const trimmed = line.endsWith("\r") ? line.slice(0, -1) : line
    if (trimmed.startsWith("data:")) {
      dataLines.push(trimmed.slice(5).trim())
    }
  }
  if (dataLines.length === 0) return null
  try {
    const parsed = JSON.parse(dataLines.join("\n"))
    if (parsed && typeof parsed === "object" && "type" in parsed && "payload" in parsed) {
      return parsed as ResearchProgressEvent
    }
  } catch {
    // Ignore malformed frames.
  }
  return null
}
