import { idempotencyKey, sdk } from "./sdk"
import { toBudgetStatus, toSpendRow } from "./mappers"
import type { BudgetUpdate } from "./types"

export const budgetsApi = {
  getStatus: async () => toBudgetStatus(await sdk.budgetsGet({})),

  update: async (body: BudgetUpdate) =>
    toBudgetStatus(
      await sdk.budgetsUpdate({
        idempotencyKey: idempotencyKey(),
        dailyLimitUsd: body.daily_limit_usd ?? null,
        weeklyLimitUsd: body.weekly_limit_usd ?? null,
        monthlyLimitUsd: body.monthly_limit_usd ?? null,
        alertThresholdPct: body.alert_threshold_pct ?? null,
        blockOnExceed: body.block_on_exceed ?? null,
      })
    ),

  setOverride: async (until: string) =>
    toBudgetStatus(await sdk.budgetsOverrideSet({ idempotencyKey: idempotencyKey(), until })),

  clearOverride: async () =>
    toBudgetStatus(await sdk.budgetsOverrideClear({ idempotencyKey: idempotencyKey() })),

  spend: async (params?: { limit?: number; since?: string }) =>
    (
      await sdk.budgetsSpend({
        since: params?.since ?? null,
        limit: params?.limit ?? 100,
      })
    ).map(toSpendRow),
}
