import { idempotencyKey, sdk } from "./sdk"
import {
  toParseCron,
  toScheduledTask,
  toSchedulerStatus,
  toTaskInbox,
  toTaskRun,
  toTaskRunDetail,
  toTaskTemplate,
} from "./mappers"
import type { ScheduledTaskCreate, ScheduledTaskUpdate } from "./types"
import type { JsonValue } from "./generated/cool_protocol"

export const tasksApi = {
  // --- Tasks ---
  list: async (params?: { enabled?: boolean }) =>
    (await sdk.tasksList({ enabled: params?.enabled ?? null })).map(toScheduledTask),
  get: async (id: number) => toScheduledTask(await sdk.tasksGet({ id })),
  create: async (body: ScheduledTaskCreate) =>
    toScheduledTask(
      await sdk.tasksCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        description: body.description ?? null,
        triggerType: body.trigger_type ?? "cron",
        cronExpression: body.cron_expression ?? null,
        intervalSeconds: body.interval_seconds ?? null,
        runAt: body.run_at ?? null,
        timezone: body.timezone ?? "UTC",
        quietHoursStart: body.quiet_hours_start ?? null,
        quietHoursEnd: body.quiet_hours_end ?? null,
        misfirePolicy: body.misfire_policy ?? "skip",
        prompt: body.prompt ?? "",
        workflowType: null,
        template: body.template ?? null,
        profileId: body.profile_id ?? null,
        model: body.model ?? null,
        toolsWhitelist: (body.tools_whitelist as unknown as JsonValue) ?? null,
        capabilityPolicy: (body.capability_policy as unknown as JsonValue) ?? null,
        workingDirectory: body.working_directory ?? null,
        approvalPolicy: body.approval_policy ?? "deny_external",
        deliveryChannels: (body.delivery_channels as unknown as JsonValue) ?? null,
        deliveryConfig: (body.delivery_config as unknown as JsonValue) ?? null,
        maxIterations: body.max_iterations ?? 10,
        maxCostPerRun: body.max_cost_per_run ?? null,
        timeoutS: body.timeout_s ?? null,
        enabled: body.enabled ?? true,
      })
    ),
  update: async (id: number, body: ScheduledTaskUpdate) =>
    toScheduledTask(
      await sdk.tasksUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        name: body.name ?? null,
        description: body.description ?? null,
        triggerType: body.trigger_type ?? null,
        cronExpression: body.cron_expression ?? null,
        intervalSeconds: body.interval_seconds ?? null,
        runAt: body.run_at ?? null,
        timezone: body.timezone ?? null,
        quietHoursStart: body.quiet_hours_start ?? null,
        quietHoursEnd: body.quiet_hours_end ?? null,
        misfirePolicy: body.misfire_policy ?? null,
        prompt: body.prompt ?? null,
        workflowType: null,
        profileId: body.profile_id ?? null,
        model: body.model ?? null,
        toolsWhitelist: (body.tools_whitelist as unknown as JsonValue) ?? null,
        capabilityPolicy: (body.capability_policy as unknown as JsonValue) ?? null,
        workingDirectory: body.working_directory ?? null,
        approvalPolicy: body.approval_policy ?? null,
        deliveryChannels: (body.delivery_channels as unknown as JsonValue) ?? null,
        deliveryConfig: (body.delivery_config as unknown as JsonValue) ?? null,
        maxIterations: body.max_iterations ?? null,
        maxCostPerRun: body.max_cost_per_run ?? null,
        timeoutS: body.timeout_s ?? null,
        enabled: body.enabled ?? null,
      })
    ),
  delete: async (id: number) => {
    await sdk.tasksDelete({ idempotencyKey: idempotencyKey(), id })
  },

  // --- Runs ---
  /** Trigger a run now; returns the running run (execution continues server-side). */
  runNow: async (id: number) =>
    toTaskRun(await sdk.tasksRun({ idempotencyKey: idempotencyKey(), id })),
  listRuns: async (id: number, params?: { limit?: number }) =>
    (await sdk.tasksRunsList({ taskId: id, limit: params?.limit ?? 50 })).map(toTaskRun),
  getRun: async (runId: number) => toTaskRunDetail(await sdk.tasksRunsGet({ id: runId })),
  cancelRun: async (runId: number) => {
    const result = await sdk.tasksRunsCancel({ idempotencyKey: idempotencyKey(), id: runId })
    return { task_run_id: result.taskRunId, cancelled: result.cancelled }
  },
  markRead: async (runId: number, isRead = true) =>
    toTaskRun(
      await sdk.tasksRunsRead({ idempotencyKey: idempotencyKey(), id: runId, isRead })
    ),

  // --- Inbox / notifications ---
  inbox: async (params?: { unread_only?: boolean; limit?: number }) =>
    toTaskInbox(
      await sdk.tasksInbox({
        unreadOnly: params?.unread_only ?? false,
        limit: params?.limit ?? 30,
      })
    ),

  // --- Helpers ---
  templates: async () => (await sdk.tasksTemplates({})).map(toTaskTemplate),
  scheduler: async () => toSchedulerStatus(await sdk.tasksScheduler({})),
  /** Natural language ("every day at 8pm") or cron -> cron + next run times. */
  parseCron: async (text: string) => toParseCron(await sdk.tasksParseCron({ text })),
}
