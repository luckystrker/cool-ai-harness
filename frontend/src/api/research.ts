import { sdk, streamRunEvents, coolApiBaseUrl, coolApiToken, idempotencyKey } from "./sdk"
import { toResearchRun, toResearchRunDetail } from "./mappers"
import type {
  ResearchProgressEvent,
  ResearchRerunRequest,
  ResearchStartRequest,
} from "./types"

/**
 * Deep research workflow API (Фаза 4).
 *
 * The whole surface is canonical: `list`/`get` read the durable rows and
 * `start`/`cancel`/`rerun` execute through the Rust research executor (M12).
 * `streamResearch` creates a run, then follows its canonical `runId` on the
 * shared cursor/reconnect event stream (`GET /api/events`), mapping the
 * `research.*`/`run.*` envelopes into the page's progress-event shape.
 * `exportUrl` stays a direct blob URL — exports are document downloads, not
 * JSON-RPC commands.
 */

export const deepResearchApi = {
  list: async (limit = 50) => (await sdk.researchList({ limit })).map(toResearchRun),
  get: async (id: number) => toResearchRunDetail(await sdk.researchGet({ id })),
  /** Start a background run; progress streams on the run's canonical events. */
  start: async (body: ResearchStartRequest) =>
    toResearchRun(
      await sdk.researchCreate({
        idempotencyKey: idempotencyKey(),
        topic: body.topic,
        depth: body.depth ?? 4,
        model: body.model ?? null,
        conversationId: body.conversation_id ?? null,
      })
    ),
  cancel: async (id: number) => sdk.researchCancel({ idempotencyKey: idempotencyKey(), id }),
  rerun: async (id: number, body: ResearchRerunRequest = {}) =>
    toResearchRun(
      await sdk.researchRerun({
        idempotencyKey: idempotencyKey(),
        id,
        depth: null,
        model: body.model ?? null,
      })
    ),
  exportUrl: (id: number, format: "md" | "html" | "pdf" | "docx") =>
    `/api/research/${id}/export?format=${format}`,
}

/**
 * Create a research run and yield live progress events from its canonical
 * run stream. Falls back to the terminal record when a record lacks
 * `runtimeRunId` (e.g. a row created before the executor landed).
 */
export async function* streamResearch(
  body: ResearchStartRequest,
  signal?: AbortSignal
): AsyncGenerator<ResearchProgressEvent> {
  const record = await sdk.researchCreate({
    idempotencyKey: idempotencyKey(),
    topic: body.topic,
    depth: body.depth ?? 4,
    model: body.model ?? null,
    conversationId: body.conversation_id ?? null,
  })
  yield { type: "started", payload: { run_id: record.id } }
  if (!record.runtimeRunId) {
    const detail = toResearchRunDetail(await sdk.researchGet({ id: record.id }))
    yield {
      type: detail.status === "completed" ? "completed" : detail.status === "cancelled" ? "cancelled" : "failed",
      payload: { error: detail.error ?? undefined },
    }
    return
  }
  for await (const envelope of streamRunEvents(record.runtimeRunId, {
    baseUrl: coolApiBaseUrl,
    signal,
    ...(coolApiToken ? { token: coolApiToken } : {}),
  })) {
    const event = toProgressEvent(envelope)
    if (event) yield event
  }
}

/** Map one canonical envelope onto the page's `{type, payload}` shape. */
function toProgressEvent(envelope: {
  event: import("./generated/cool_protocol").CanonicalEvent
}): ResearchProgressEvent | null {
  const { kind, payload } = envelope.event
  switch (kind) {
    case "research.stage":
      return { type: "stage", payload: { stage: payload.stage, message: payload.message } }
    case "research.subquestion_started":
      return {
        type: "subquestion_started",
        payload: { index: payload.index, sub_question: payload.question },
      }
    case "research.subquestion_completed":
      return {
        type: "subquestion_completed",
        payload: { index: payload.index, status: payload.status },
      }
    case "research.source_found":
      return {
        type: "source_found",
        payload: { url: payload.url, title: payload.title ?? "", snippet: payload.snippet },
      }
    case "research.completed":
      return { type: "completed", payload: { source_count: payload.sourceCount } }
    case "research.failed":
      return { type: "failed", payload: { error: payload.error ?? "Research failed" } }
    case "research.cancelled":
      return { type: "cancelled", payload: {} }
    case "run.completed":
      // The executor emits research.completed first; this is a safety terminal.
      return { type: "completed", payload: {} }
    case "run.failed":
      return { type: "failed", payload: { error: payload.reason } }
    case "run.cancelled":
      return { type: "cancelled", payload: {} }
    default:
      return null
  }
}
