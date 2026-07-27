import { api } from "./client"
import type {
  SubagentLaunchBatchRequest,
  SubagentLaunchRequest,
  SubagentRole,
  SubagentRoleCreate,
  SubagentRoleUpdate,
  SubagentRun,
  SubagentRunDetail,
} from "./types"

export const subagentsApi = {
  // --- Tools ---
  listTools: () => api.get<string[]>("/api/subagents/tools"),

  // --- Roles ---
  listRoles: () => api.get<SubagentRole[]>("/api/subagents/roles"),
  getRole: (id: number) => api.get<SubagentRole>(`/api/subagents/roles/${id}`),
  createRole: (body: SubagentRoleCreate) =>
    api.post<SubagentRole>("/api/subagents/roles", body),
  updateRole: (id: number, body: SubagentRoleUpdate) =>
    api.put<SubagentRole>(`/api/subagents/roles/${id}`, body),
  deleteRole: (id: number) => api.delete<void>(`/api/subagents/roles/${id}`),

  // --- Runs ---
  launch: (body: SubagentLaunchRequest) =>
    api.post<SubagentRun>("/api/subagents/launch", body),
  launchBatch: (body: SubagentLaunchBatchRequest) =>
    api.post<SubagentRun[]>("/api/subagents/launch-batch", body),
  listRuns: (params?: { parent_conversation_id?: number; status?: string }) => {
    const qs = new URLSearchParams()
    if (params?.parent_conversation_id != null)
      qs.set("parent_conversation_id", String(params.parent_conversation_id))
    if (params?.status) qs.set("status", params.status)
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<SubagentRun[]>(`/api/subagents/runs${suffix}`)
  },
  getRun: (id: number) => api.get<SubagentRunDetail>(`/api/subagents/runs/${id}`),
  cancelRun: (id: number) =>
    api.post<{ run_id: number; cancelled: boolean }>(`/api/subagents/runs/${id}/cancel`),
  deleteRun: (id: number) => api.delete<void>(`/api/subagents/runs/${id}`),
}
