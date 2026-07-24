import { api } from "./client"
import type {
  BudgetStatusResponse,
  BudgetUpdate,
  SpendRow,
} from "./types"

export const budgetsApi = {
  getStatus: () => api.get<BudgetStatusResponse>("/api/budgets"),

  update: (body: BudgetUpdate) => api.put<BudgetStatusResponse>("/api/budgets", body),

  setOverride: (until: string) =>
    api.post<BudgetStatusResponse>("/api/budgets/override", { until }),

  clearOverride: () => api.delete<BudgetStatusResponse>("/api/budgets/override"),

  spend: (params?: { limit?: number; since?: string }) => {
    const qs = new URLSearchParams()
    if (params?.limit) qs.set("limit", String(params.limit))
    if (params?.since) qs.set("since", params.since)
    const suffix = qs.toString() ? `?${qs.toString()}` : ""
    return api.get<SpendRow[]>(`/api/budgets/spend${suffix}`)
  },
}
