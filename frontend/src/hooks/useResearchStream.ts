import { useCallback, useRef, useState } from "react"
import { streamResearch } from "@/api/research"
import type { ResearchProgressEvent, ResearchStartRequest } from "@/api/types"

export interface ResearchProgressState {
  /** Research run id (from the `started` event). */
  runId: number | null
  stage: "decompose" | "gather" | "synthesize" | null
  subQuestions: { index: number; text: string; status: "pending" | "running" | "completed" | "failed" | "empty" }[]
  sources: { url: string; title: string }[]
  terminal: "completed" | "failed" | "cancelled" | null
  error: string | null
}

const initialState: ResearchProgressState = {
  runId: null,
  stage: null,
  subQuestions: [],
  sources: [],
  terminal: null,
  error: null,
}

/**
 * Drives one deep-research SSE stream and accumulates live progress state.
 *
 * `start` fires the request and consumes events until the terminal one;
 * `abort` cancels the underlying fetch (the run continues server-side until
 * cancelled via the cancel endpoint — the page calls deepResearchApi.cancel
 * with `runId`).
 */
export function useResearchStream() {
  const [progress, setProgress] = useState<ResearchProgressState>(initialState)
  const controllerRef = useRef<AbortController | null>(null)

  const applyEvent = useCallback((event: ResearchProgressEvent) => {
    const { type, payload } = event
    switch (type) {
      case "started":
        setProgress((p) => ({ ...p, runId: payload.run_id as number }))
        break
      case "stage":
        setProgress((p) => ({ ...p, stage: payload.stage as ResearchProgressState["stage"] }))
        break
      case "subquestion_started":
        setProgress((p) => {
          const index = payload.index as number
          const exists = p.subQuestions.some((q) => q.index === index)
          const entry = { index, text: (payload.sub_question as string) ?? "", status: "running" as const }
          return {
            ...p,
            subQuestions: exists
              ? p.subQuestions.map((q) => (q.index === index ? { ...q, status: "running" } : q))
              : [...p.subQuestions, entry],
          }
        })
        break
      case "subquestion_completed":
        setProgress((p) => ({
          ...p,
          subQuestions: p.subQuestions.map((q) =>
            q.index === (payload.index as number)
              ? { ...q, status: (payload.status as "completed" | "failed" | "empty") ?? "completed" }
              : q
          ),
        }))
        break
      case "source_found":
        setProgress((p) => ({
          ...p,
          sources: [...p.sources, { url: payload.url as string, title: (payload.title as string) ?? "" }],
        }))
        break
      case "completed":
      case "failed":
      case "cancelled":
        setProgress((p) => ({
          ...p,
          terminal: type,
          error: type === "failed" ? ((payload.error as string) ?? "Research failed") : p.error,
        }))
        break
      default:
        break
    }
  }, [])

  const start = useCallback(
    async (body: ResearchStartRequest) => {
      controllerRef.current?.abort()
      setProgress(initialState)
      const controller = new AbortController()
      controllerRef.current = controller
      for await (const event of streamResearch(body, controller.signal)) {
        applyEvent(event)
      }
    },
    [applyEvent]
  )

  const abort = useCallback(() => {
    controllerRef.current?.abort()
  }, [])

  const reset = useCallback(() => {
    controllerRef.current?.abort()
    setProgress(initialState)
  }, [])

  return { progress, start, abort, reset }
}
