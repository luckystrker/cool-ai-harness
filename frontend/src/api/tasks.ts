import { api } from "./client"
import type {
  ParseCronResponse,
  ScheduledTask,
  ScheduledTaskCreate,
  ScheduledTaskUpdate,
  SchedulerStatus,
  TaskInbox,
  TaskRun,
  TaskRunDetail,
  TaskTemplate,
} from "./types"

export const tasksApi = {
  // --- Tasks ---
  list: (params?: { enabled?: boolean }) => {
    const qs = new URLSearchParams()
    if (params?.enabled != null) qs.set("enabled", String(params.enabled))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<ScheduledTask[]>(`/api/tasks${suffix}`)
  },
  get: (id: number) => api.get<ScheduledTask>(`/api/tasks/${id}`),
  create: (body: ScheduledTaskCreate) => api.post<ScheduledTask>("/api/tasks", body),
  update: (id: number, body: ScheduledTaskUpdate) =>
    api.put<ScheduledTask>(`/api/tasks/${id}`, body),
  delete: (id: number) => api.delete<void>(`/api/tasks/${id}`),

  // --- Runs ---
  /** Trigger a run now; returns the queued run (execution continues server-side). */
  runNow: (id: number) => api.post<TaskRun>(`/api/tasks/${id}/run`),
  listRuns: (id: number, params?: { limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<TaskRun[]>(`/api/tasks/${id}/runs${suffix}`)
  },
  getRun: (runId: number) => api.get<TaskRunDetail>(`/api/tasks/runs/${runId}`),
  cancelRun: (runId: number) =>
    api.post<{ task_run_id: number; cancelled: boolean }>(
      `/api/tasks/runs/${runId}/cancel`
    ),
  markRead: (runId: number, isRead = true) =>
    api.post<TaskRun>(`/api/tasks/runs/${runId}/read`, { is_read: isRead }),

  // --- Inbox / notifications ---
  inbox: (params?: { unread_only?: boolean; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.unread_only) qs.set("unread_only", "true")
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<TaskInbox>(`/api/tasks/inbox${suffix}`)
  },

  // --- Helpers ---
  templates: () => api.get<TaskTemplate[]>("/api/tasks/templates"),
  scheduler: () => api.get<SchedulerStatus>("/api/tasks/scheduler"),
  /** Natural language ("every day at 8pm") or cron -> cron + next run times. */
  parseCron: (text: string) => api.post<ParseCronResponse>("/api/tasks/parse-cron", { text }),
}
