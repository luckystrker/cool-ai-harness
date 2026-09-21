import { idempotencyKey, sdk } from "./sdk"
import { toMacroTool, toToolCatalogItem } from "./mappers"
import type { MacroToolCreate } from "./types"
import type { JsonValue } from "./generated/cool_protocol"

export const constructorApi = {
  // The canonical runtime catalog (`tools.list`) is the single picker source for
  // profile/role tool_names. Agent-constructor macros still execute in Python,
  // so a Python-only tool name (network/ML/browser) is not offered here.
  tools: async () => (await sdk.toolsList({})).map(toToolCatalogItem),
  macros: async () =>
    (await sdk.constructorMacros({ includeInactive: false })).map(toMacroTool),
  createMacro: async (body: MacroToolCreate) =>
    toMacroTool(
      await sdk.constructorMacrosCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        description: body.description ?? "",
        inputSchema: (body.input_schema as unknown as JsonValue) ?? {},
        steps: body.steps as unknown as JsonValue,
        isActive: true,
      })
    ),
  updateMacro: async (id: number, body: Partial<MacroToolCreate> & { is_active?: boolean }) =>
    toMacroTool(
      await sdk.constructorMacrosUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        description: body.description ?? null,
        inputSchema: (body.input_schema as unknown as JsonValue) ?? null,
        steps: (body.steps as unknown as JsonValue) ?? null,
        isActive: body.is_active ?? null,
      })
    ),
  deleteMacro: async (id: number) =>
    sdk.constructorMacrosDelete({ idempotencyKey: idempotencyKey(), id }),
}
