import { api } from "./client"
import type {
  AnalyticsSummary,
  CallHistoryResponse,
  LatencyPoint,
  MemoryActivityPoint,
  ModelSpend,
  SpendTimeSeriesPoint,
  TopTool,
} from "./types"

export const analyticsApi = {
  summary: (days = 30) => api.get<AnalyticsSummary>(`/api/analytics/summary?days=${days}`),

  spendOverTime: (days = 30, bucket: "day" | "hour" = "day") =>
    api.get<SpendTimeSeriesPoint[]>(`/api/analytics/spend-over-time?days=${days}&bucket=${bucket}`),

  spendByModel: (days = 30) =>
    api.get<ModelSpend[]>(`/api/analytics/spend-by-model?days=${days}`),

  topTools: (days = 30, limit = 20) =>
    api.get<TopTool[]>(`/api/analytics/top-tools?days=${days}&limit=${limit}`),

  latency: (days = 30, bucket: "day" | "hour" = "day") =>
    api.get<LatencyPoint[]>(`/api/analytics/latency?days=${days}&bucket=${bucket}`),

  callHistory: (params?: { limit?: number; offset?: number; model?: string; provider?: string }) => {
    const qs = new URLSearchParams()
    if (params?.limit) qs.set("limit", String(params.limit))
    if (params?.offset) qs.set("offset", String(params.offset))
    if (params?.model) qs.set("model", params.model)
    if (params?.provider) qs.set("provider", params.provider)
    const suffix = qs.toString() ? `?${qs.toString()}` : ""
    return api.get<CallHistoryResponse>(`/api/analytics/call-history${suffix}`)
  },

  memoryActivity: (days = 30, bucket: "day" | "hour" = "day") =>
    api.get<MemoryActivityPoint[]>(`/api/analytics/memory-activity?days=${days}&bucket=${bucket}`),
}
