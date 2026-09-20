import {
  coolApiBaseUrl,
  coolApiToken,
  idempotencyKey,
  sdk,
  streamRunEvents,
} from "./sdk"
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

  /** Start an approved plan's execution and return its durable run. */
  execute: async (conversationId: number, planId: number) =>
    sdk.plansExecute({ idempotencyKey: idempotencyKey(), conversationId, planId }),

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
 * `plans.execute` starts a durable canonical run; its `plan.*` events are
 * streamed over the canonical cursor/reconnect transport and re-projected into
 * the legacy `{kind, payload}` shape the plan card consumes.
 */
export async function* executePlan(
  conversationId: number,
  planId: number,
  signal?: AbortSignal
): AsyncGenerator<PlanExecuteEvent> {
  const { runId } = await sdk.plansExecute({
    idempotencyKey: idempotencyKey(),
    conversationId,
    planId,
  })
  for await (const envelope of streamRunEvents(runId, {
    baseUrl: coolApiBaseUrl,
    signal,
    ...(coolApiToken ? { token: coolApiToken } : {}),
  })) {
    const event = envelope.event
    switch (event.kind) {
      case "plan.step_started":
      case "plan.step_completed":
        yield {
          kind: event.kind === "plan.step_started" ? "plan_step_start" : "plan_step_complete",
          payload: {
            plan_id: event.payload.planId,
            position: event.payload.position,
            title: event.payload.title,
            status: event.payload.status,
            result_summary: event.payload.resultSummary,
          },
        }
        break
      case "plan.progress":
        yield {
          kind: "plan_progress",
          payload: {
            plan_id: event.payload.planId,
            completed: event.payload.completedSteps,
            total: event.payload.totalSteps,
            status: event.payload.status,
          },
        }
        break
      case "run.completed":
      case "run.failed":
      case "run.cancelled":
        // Terminal signal so the card does not stay stuck in `executing` when
        // a step failed and left later steps pending.
        yield { kind: event.kind.replace(".", "_"), payload: {} }
        break
      default:
        break
    }
  }
}

export type { Plan, PlanTemplate }
