import { api } from "./client"
import type { Plan, PlanTemplate } from "./types"

/** API client for Planning Mode endpoints (Фаза 2 §1). */
export const plansApi = {
  /** List plans for a conversation, newest first. */
  list: (conversationId: number) =>
    api.get<Plan[]>(`/api/conversations/${conversationId}/plans`),

  /** Get plan detail with steps. */
  get: (conversationId: number, planId: number) =>
    api.get<Plan>(`/api/conversations/${conversationId}/plans/${planId}`),

  /** Edit a draft plan's title and/or steps. */
  update: (
    conversationId: number,
    planId: number,
    body: { title?: string; steps?: { position: number; title: string; description?: string; depends_on?: number[]; tools?: string[] }[] }
  ) =>
    api.patch<Plan>(`/api/conversations/${conversationId}/plans/${planId}`, body),

  /** Approve or reject a draft plan. */
  approve: (conversationId: number, planId: number, approved: boolean) =>
    api.post<Plan>(`/api/conversations/${conversationId}/plans/${planId}/approve`, { approved }),

  /** Cancel a plan. */
  cancel: (conversationId: number, planId: number) =>
    api.post<Plan>(`/api/conversations/${conversationId}/plans/${planId}/cancel`),

  /** Execute an approved plan (returns SSE stream URL for manual handling). */
  executeUrl: (conversationId: number, planId: number) =>
    `/api/conversations/${conversationId}/plans/${planId}/execute`,

  // --- Templates ---

  /** List all plan templates. */
  listTemplates: () => api.get<PlanTemplate[]>("/api/plan-templates"),

  /** Create a new plan template. */
  createTemplate: (body: { name: string; description?: string; steps: unknown[] }) =>
    api.post<PlanTemplate>("/api/plan-templates", body),

  /** Delete a plan template. */
  deleteTemplate: (templateId: number) =>
    api.delete<{ deleted: number }>(`/api/plan-templates/${templateId}`),
}
