/** API functions for the Inspector (Фаза 1.5 §6). */

import { idempotencyKey, sdk } from "./sdk"
import { toAgentRun, toReplayResponse, toRunComparison, toRunTimeline } from "./mappers"
import type { ReplayRequest } from "./types"

/** List the durable legacy runs of a conversation, newest first. */
export async function listRuns(convId: number) {
  const records = await sdk.runsList({ conversationId: convId, beforeId: null, limit: 100 })
  return records.map(toAgentRun)
}

/** Get the structured per-iteration timeline for a run. */
export async function getRunTimeline(_convId: number, runId: number) {
  return toRunTimeline(await sdk.inspectorTimeline({ id: runId }))
}

/** Compare two runs side-by-side. */
export async function compareRuns(aId: number, bId: number) {
  return toRunComparison(await sdk.inspectorCompare({ leftRunId: aId, rightRunId: bId }))
}

/** Replay a run with optional overrides. */
export async function replayRun(_convId: number, runId: number, overrides?: ReplayRequest) {
  return toReplayResponse(
    await sdk.inspectorReplay({
      idempotencyKey: idempotencyKey(),
      runId,
      model: overrides?.model ?? null,
      systemPrompt: overrides?.system_prompt ?? null,
      temperature: overrides?.temperature ?? null,
    })
  )
}
