import { idempotencyKey, sdk } from "./sdk"
import { toPlan, toPlanTemplate } from "./mappers"
import type { JsonValue } from "./generated/cool_protocol"
import type { Plan, PlanTemplate } from "./types"

/** API client for Planning Mode endpoints (Фаза 2 §1). */
export const plansApi = {
  /** List plans for a conversation, newest first. */
  list: async (conversationId: number) =>
    (await sdk.plansList({ conversationId })).map(toPlan),

  /** Get plan detail with steps. */
  get: async (conversationId: number, planId: number) =>
    toPlan(await sdk.plansGet({ conversationId, planId })),

  /** Edit a draft plan's title and/or steps. */
  update: async (
    conversationId: number,
    planId: number,
    body: {
      title?: string
      steps?: {
        position: number
        title: string
        description?: string
        depends_on?: number[]
        tools?: string[]
      }[]
    }
  ) =>
    toPlan(
      await sdk.plansUpdate({
        idempotencyKey: idempotencyKey(),
        conversationId,
        planId,
        title: body.title ?? null,
        steps: (body.steps as unknown as JsonValue) ?? null,
      })
    ),

  /** Approve or reject a draft plan. */
  approve: async (conversationId: number, planId: number, approved: boolean) =>
    toPlan(
      await sdk.plansApprove({
        idempotencyKey: idempotencyKey(),
        conversationId,
        planId,
        approved,
      })
    ),

  /** Cancel a plan. */
  cancel: async (conversationId: number, planId: number) =>
    toPlan(
      await sdk.plansCancel({ idempotencyKey: idempotencyKey(), conversationId, planId })
    ),

  /** Execute an approved plan (returns SSE stream URL for manual handling). */
  executeUrl: (conversationId: number, planId: number) =>
    `/api/conversations/${conversationId}/plans/${planId}/execute`,

  // --- Templates ---

  /** List all plan templates. */
  listTemplates: async () => (await sdk.plansTemplatesList({})).map(toPlanTemplate),

  /** Create a new plan template. */
  createTemplate: async (body: { name: string; description?: string; steps: unknown[] }) =>
    toPlanTemplate(
      await sdk.plansTemplatesCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        description: body.description ?? null,
        steps: body.steps as unknown as JsonValue,
      })
    ),

  /** Delete a plan template. */
  deleteTemplate: async (templateId: number) =>
    sdk.plansTemplatesDelete({ idempotencyKey: idempotencyKey(), id: templateId }),
}

export interface PlanExecuteEvent {
  kind: string
  payload: Record<string, unknown>
}

/**
 * Stream an approved plan's execution.
 *
 * Plan execution still runs in the Python runtime (the canonical runtime has
 * no plan executor yet), so this stays on the per-plan SSE transport and is a
 * documented `sse/stream` exception in the protocol inventory.
 */
export async function* executePlan(
  conversationId: number,
  planId: number,
  signal?: AbortSignal
): AsyncGenerator<PlanExecuteEvent> {
  const resp = await fetch(
    `/api/conversations/${conversationId}/plans/${planId}/execute`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "text/event-stream" },
      signal,
    }
  )
  if (!resp.ok || !resp.body) {
    throw new Error(`Execution failed (${resp.status})`)
  }
  const reader = resp.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      let sepIdx: number
      while ((sepIdx = buffer.indexOf("\n\n")) !== -1) {
        const frame = buffer.slice(0, sepIdx)
        buffer = buffer.slice(sepIdx + 2)
        const dataLine = frame.split("\n").find((line) => line.startsWith("data:"))
        if (!dataLine) continue
        try {
          const parsed = JSON.parse(dataLine.slice(5).trim())
          const payload = parsed?.payload ?? parsed
          const kind = parsed?.kind ?? ""
          yield { kind, payload: (payload ?? {}) as Record<string, unknown> }
        } catch {
          // Skip malformed frames.
        }
      }
    }
  } finally {
    reader.releaseLock()
  }
}

export type { Plan, PlanTemplate }
