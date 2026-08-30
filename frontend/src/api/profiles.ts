import { api } from "./client"
import type { AgentProfile, ProfileCreate, ProfileUpdate } from "./types"

export const profilesApi = {
  list: (includeInactive = false) =>
    api.get<AgentProfile[]>(
      `/api/profiles${includeInactive ? "?include_inactive=true" : ""}`
    ),

  get: (id: number) => api.get<AgentProfile>(`/api/profiles/${id}`),

  create: (body: ProfileCreate) => api.post<AgentProfile>("/api/profiles", body),

  update: (id: number, body: ProfileUpdate) =>
    api.patch<AgentProfile>(`/api/profiles/${id}`, body),

  delete: (id: number) => api.delete<{ deleted: number }>(`/api/profiles/${id}`),

  seed: () => api.post<{ created: number }>("/api/profiles/seed"),

  clone: (id: number) => api.post<AgentProfile>(`/api/profiles/${id}/clone`),

  playground: (id: number, body: { title?: string; initial_prompt?: string } = {}) =>
    api.post<{ conversation_id: number }>(`/api/profiles/${id}/playground`, body),
}
