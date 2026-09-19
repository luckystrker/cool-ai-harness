import { sdk } from "./sdk"
import {
  toAnalyticsSummary,
  toCallHistoryResponse,
  toLatencyPoint,
  toMemoryActivityPoint,
  toModelSpend,
  toSpendTimeSeriesPoint,
  toTopTool,
} from "./mappers"

export const analyticsApi = {
  summary: async (days = 30) => toAnalyticsSummary(await sdk.analyticsSummary({ days })),

  spendOverTime: async (days = 30, bucket: "day" | "hour" = "day") =>
    (await sdk.analyticsSpendOverTime({ days, bucket })).map(toSpendTimeSeriesPoint),

  spendByModel: async (days = 30) =>
    (await sdk.analyticsSpendByModel({ days })).map(toModelSpend),

  topTools: async (days = 30, limit = 20) =>
    (await sdk.analyticsTopTools({ days, limit })).map(toTopTool),

  latency: async (days = 30, bucket: "day" | "hour" = "day") =>
    (await sdk.analyticsLatency({ days, bucket })).map(toLatencyPoint),

  callHistory: async (params?: {
    limit?: number
    offset?: number
    model?: string
    provider?: string
  }) =>
    toCallHistoryResponse(
      await sdk.analyticsCallHistory({
        limit: params?.limit ?? 100,
        offset: params?.offset ?? 0,
        model: params?.model ?? null,
        provider: params?.provider ?? null,
      })
    ),

  memoryActivity: async (days = 30, bucket: "day" | "hour" = "day") =>
    (await sdk.analyticsMemoryActivity({ days, bucket })).map(toMemoryActivityPoint),
}
