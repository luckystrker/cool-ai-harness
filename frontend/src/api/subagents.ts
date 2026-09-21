import { idempotencyKey, sdk } from "./sdk"
import { toSubagentRole, toSubagentRun, toSubagentRunDetail } from "./mappers"
import type {
  SubagentLaunchBatchRequest,
  SubagentLaunchRequest,
  SubagentRoleCreate,
  SubagentRoleUpdate,
} from "./types"
import type { JsonValue } from "./generated/cool_protocol"

export const subagentsApi = {
  // --- Tools ---
  // Role tool names are executed by the Rust subagent runtime, so the picker
  // uses the canonical runtime catalog (`tools.list`).
  listTools: async () => (await sdk.toolsList({})).map((tool) => tool.name),

  // --- Roles ---
  listRoles: async () => (await sdk.subagentsRolesList({})).map(toSubagentRole),
  getRole: async (id: number) => toSubagentRole(await sdk.subagentsRolesGet({ id })),
  createRole: async (body: SubagentRoleCreate) =>
    toSubagentRole(
      await sdk.subagentsRolesCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        description: body.description ?? null,
        systemPrompt: body.system_prompt ?? null,
        model: body.model ?? null,
        toolNames: (body.tool_names as unknown as JsonValue) ?? null,
        capabilityPolicy: (body.capability_policy as unknown as JsonValue) ?? null,
        maxIterations: body.max_iterations ?? 10,
        maxCostUsd: body.max_cost_usd ?? null,
        isBuiltin: false,
      })
    ),
  updateRole: async (id: number, body: SubagentRoleUpdate) =>
    toSubagentRole(
      await sdk.subagentsRolesUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        name: body.name ?? null,
        description: body.description ?? null,
        systemPrompt: body.system_prompt ?? null,
        model: body.model ?? null,
        toolNames: (body.tool_names as unknown as JsonValue) ?? null,
        capabilityPolicy: (body.capability_policy as unknown as JsonValue) ?? null,
        maxIterations: body.max_iterations ?? null,
        maxCostUsd: body.max_cost_usd ?? null,
        clearMaxCostUsd: body.max_cost_usd === null,
      })
    ),
  deleteRole: async (id: number) => {
    await sdk.subagentsRolesDelete({ idempotencyKey: idempotencyKey(), id })
  },

  // --- Runs ---
  launch: async (body: SubagentLaunchRequest) =>
    toSubagentRun(
      await sdk.subagentsLaunch({
        idempotencyKey: idempotencyKey(),
        parentConversationId: body.parent_conversation_id,
        roleId: body.role_id ?? null,
        profileId: null,
        parentRunId: null,
        name: body.name ?? null,
        prompt: body.prompt,
        model: body.model ?? null,
      })
    ),
  launchBatch: async (body: SubagentLaunchBatchRequest) =>
    (
      await sdk.subagentsLaunchBatch({
        idempotencyKey: idempotencyKey(),
        parentConversationId: body.parent_conversation_id,
        items: body.items.map((item) => ({
          roleId: item.role_id ?? null,
          profileId: null,
          name: item.name ?? null,
          prompt: item.prompt,
          model: item.model ?? null,
        })),
      })
    ).map(toSubagentRun),
  listRuns: async (params?: { parent_conversation_id?: number; status?: string }) =>
    (
      await sdk.subagentsRunsList({
        parentConversationId: params?.parent_conversation_id ?? null,
        status: params?.status ?? null,
        limit: 50,
      })
    ).map(toSubagentRun),
  getRun: async (id: number) => toSubagentRunDetail(await sdk.subagentsRunsGet({ id })),
  cancelRun: async (id: number) => {
    const result = await sdk.subagentsRunsCancel({ idempotencyKey: idempotencyKey(), id })
    return { run_id: result.runId, cancelled: result.cancelled }
  },
  deleteRun: async (id: number) => {
    await sdk.subagentsRunsDelete({ idempotencyKey: idempotencyKey(), id })
  },
}
