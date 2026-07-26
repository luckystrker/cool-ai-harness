import { api } from "./client"
import type { SkillCreateRequest, SkillCreateResponse, SkillListResponse } from "./types"

export const skillsApi = {
  /** List all available skills, optionally filtered by source. */
  list: (source?: string) =>
    api.get<SkillListResponse>(`/api/skills${source ? `?source=${encodeURIComponent(source)}` : ""}`),

  /** Create a new skill. */
  create: (body: SkillCreateRequest) => api.post<SkillCreateResponse>("/api/skills", body),

  /** Delete a user-created skill. */
  delete: (name: string) => api.delete<void>(`/api/skills/${encodeURIComponent(name)}`),
}
