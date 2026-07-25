/** API functions for the Inspector (Фаза 1.5 §6). */

import { api } from "./client"
import type { ReplayRequest, ReplayResponse, RunComparison, RunTimeline } from "./types"

/** Get the structured per-iteration timeline for a run. */
export function getRunTimeline(convId: number, runId: number) {
  return api.get<RunTimeline>(`/api/conversations/${convId}/runs/${runId}/timeline`)
}

/** Compare two runs side-by-side. */
export function compareRuns(aId: number, bId: number) {
  return api.get<RunComparison>(`/api/runs/compare?a=${aId}&b=${bId}`)
}

/** Replay a run with optional overrides. */
export function replayRun(convId: number, runId: number, overrides?: ReplayRequest) {
  return api.post<ReplayResponse>(`/api/conversations/${convId}/runs/${runId}/replay`, overrides ?? {})
}
