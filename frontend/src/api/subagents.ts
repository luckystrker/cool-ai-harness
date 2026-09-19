import { api } from "./client"
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
  listTools: () => api.get<string[]>("/api/subagents/tools"),

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
  launch: (body: SubagentLaunchRequest) =>
    api.post<import("./types").SubagentRun>("/api/subagents/launch", body),
  launchBatch: (body: SubagentLaunchBatchRequest) =>
    api.post<import("./types").SubagentRun[]>("/api/subagents/launch-batch", body),
  listRuns: async (params?: { parent_conversation_id?: number; status?: string }) =>
    (
      await sdk.subagentsRunsList({
        parentConversationId: params?.parent_conversation_id ?? null,
        status: params?.status ?? null,
        limit: 50,
      })
    ).map(toSubagentRun),
  getRun: async (id: number) => toSubagentRunDetail(await sdk.subagentsRunsGet({ id })),
  cancelRun: (id: number) =>
    api.post<{ run_id: number; cancelled: boolean }>(`/api/subagents/runs/${id}/cancel`),
  deleteRun: async (id: number) => {
    await sdk.subagentsRunsDelete({ idempotencyKey: idempotencyKey(), id })
  },
}
