import { api } from "./client"
import type { WebhookEndpoint, WebhookEvent } from "./types"

export const webhooksApi = {
  // --- Endpoints ---
  list: () => api.get<WebhookEndpoint[]>("/api/webhooks"),
  get: (id: number) => api.get<WebhookEndpoint>(`/api/webhooks/${id}`),
  create: (body: {
    name: string
    source_type?: string
    event_filter?: string[] | null
    task_id?: number | null
    prompt_template?: string | null
    enabled?: boolean
  }) => api.post<WebhookEndpoint>("/api/webhooks", body),
  update: (id: number, body: Partial<{
    name: string
    source_type: string
    event_filter: string[] | null
    task_id: number | null
    prompt_template: string | null
    enabled: boolean
  }>) => api.put<WebhookEndpoint>(`/api/webhooks/${id}`, body),
  delete: (id: number) => api.delete<void>(`/api/webhooks/${id}`),

  // --- Events ---
  listEvents: (endpointId: number, params?: { status?: string; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.status) qs.set("status", params.status)
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<WebhookEvent[]>(`/api/webhooks/${endpointId}/events${suffix}`)
  },
  replay: (endpointId: number, eventId: number) =>
    api.post<WebhookEvent>(`/api/webhooks/${endpointId}/events/${eventId}/replay`),
}
