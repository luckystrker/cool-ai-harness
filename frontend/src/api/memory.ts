import { api } from "./client"
import type {
  Entity,
  EntityCreate,
  EntityUpdate,
  Episode,
  MemoryCreate,
  MemoryExplain,
  MemoryExtractRequest,
  MemoryExtractResponse,
  MemoryItem,
  MemoryStats,
  MemoryUpdate,
} from "./types"

export const memoryApi = {
  // --- Memories ---
  list: (params?: {
    memory_type?: string
    scope?: string
    status?: string
    limit?: number
    offset?: number
  }) => {
    const qs = new URLSearchParams()
    if (params?.memory_type) qs.set("memory_type", params.memory_type)
    if (params?.scope) qs.set("scope", params.scope)
    if (params?.status) qs.set("status", params.status)
    if (params?.limit != null) qs.set("limit", String(params.limit))
    if (params?.offset != null) qs.set("offset", String(params.offset))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<MemoryItem[]>(`/api/memory${suffix}`)
  },
  get: (id: number) => api.get<MemoryItem>(`/api/memory/${id}`),
  create: (body: MemoryCreate) => api.post<MemoryItem>("/api/memory", body),
  update: (id: number, body: MemoryUpdate) =>
    api.patch<MemoryItem>(`/api/memory/${id}`, body),
  delete: (id: number, hard = false) =>
    api.delete<void>(`/api/memory/${id}?hard=${hard}`),

  // --- Confirmation workflow ---
  listPending: () => api.get<MemoryItem[]>("/api/memory/pending"),
  confirm: (id: number) => api.post<MemoryItem>(`/api/memory/${id}/confirm`),
  reject: (id: number) => api.post<void>(`/api/memory/${id}/reject`),
  pin: (id: number, pinned: boolean) =>
    api.post<MemoryItem>(`/api/memory/${id}/pin`, { pinned }),

  // --- Explainability ---
  explain: (id: number) => api.get<MemoryExplain>(`/api/memory/${id}/explain`),

  // --- Export (triggers a browser download) ---
  exportMemories: async (
    format: "json" | "markdown" = "json",
    includeArchived = false
  ): Promise<void> => {
    const qs = new URLSearchParams({ format })
    if (includeArchived) qs.set("include_archived", "true")
    const resp = await fetch(`/api/memory/export?${qs}`)
    if (!resp.ok) throw new Error(`Export failed: ${resp.status}`)
    const blob = await resp.blob()
    const url = URL.createObjectURL(blob)
    const a = document.createElement("a")
    a.href = url
    a.download = format === "json" ? "memories.json" : "memories.md"
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)
  },

  // --- Episodes ---
  listEpisodes: (params?: { agent_id?: number; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.agent_id != null) qs.set("agent_id", String(params.agent_id))
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<Episode[]>(`/api/memory/episodes${suffix}`)
  },

  // --- Stats ---
  stats: () => api.get<MemoryStats>("/api/memory/stats"),

  // --- Extraction ---
  extract: (body: MemoryExtractRequest) =>
    api.post<MemoryExtractResponse>("/api/memory/extract", body),
}

export const entitiesApi = {
  list: (params?: { entity_type?: string; query?: string; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.entity_type) qs.set("entity_type", params.entity_type)
    if (params?.query) qs.set("query", params.query)
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<Entity[]>(`/api/entities${suffix}`)
  },
  get: (id: number) => api.get<Entity>(`/api/entities/${id}`),
  create: (body: EntityCreate) => api.post<Entity>("/api/entities", body),
  update: (id: number, body: EntityUpdate) =>
    api.patch<Entity>(`/api/entities/${id}`, body),
  delete: (id: number) => api.delete<void>(`/api/entities/${id}`),
}
