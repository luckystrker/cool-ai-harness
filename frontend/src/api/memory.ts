import { api } from "./client"
import type {
  Episode,
  MemoryCreate,
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
