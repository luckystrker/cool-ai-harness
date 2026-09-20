import { idempotencyKey, sdk } from "./sdk"
import { toWebhookEndpoint, toWebhookEvent } from "./mappers"
import type { JsonValue } from "./generated/cool_protocol"

export const webhooksApi = {
  // --- Endpoints ---
  list: async () => (await sdk.webhooksList({})).map(toWebhookEndpoint),
  get: async (id: number) => toWebhookEndpoint(await sdk.webhooksGet({ id })),
  create: async (body: {
    name: string
    source_type?: string
    event_filter?: string[] | null
    task_id?: number | null
    prompt_template?: string | null
    enabled?: boolean
  }) =>
    toWebhookEndpoint(
      await sdk.webhooksCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        sourceType: body.source_type ?? null,
        eventFilter: (body.event_filter as unknown as JsonValue) ?? null,
        taskId: body.task_id ?? null,
        promptTemplate: body.prompt_template ?? null,
        enabled: body.enabled ?? null,
      })
    ),
  update: async (
    id: number,
    body: Partial<{
      name: string
      source_type: string
      event_filter: string[] | null
      task_id: number | null
      prompt_template: string | null
      enabled: boolean
    }>
  ) =>
    toWebhookEndpoint(
      await sdk.webhooksUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        name: body.name ?? null,
        sourceType: body.source_type ?? null,
        eventFilter: (body.event_filter as unknown as JsonValue) ?? null,
        taskId: body.task_id ?? null,
        promptTemplate: body.prompt_template ?? null,
        enabled: body.enabled ?? null,
      })
    ),
  delete: async (id: number) => {
    await sdk.webhooksDelete({ idempotencyKey: idempotencyKey(), id })
  },

  // --- Events ---
  listEvents: async (
    endpointId: number,
    params?: { status?: string; limit?: number }
  ) =>
    (
      await sdk.webhooksEvents({
        endpointId,
        status: params?.status ?? null,
        limit: params?.limit ?? 50,
      })
    ).map(toWebhookEvent),
  replay: async (endpointId: number, eventId: number) =>
    toWebhookEvent(
      await sdk.webhooksReplay({
        idempotencyKey: idempotencyKey(),
        endpointId,
        eventId,
      })
    ),
}
