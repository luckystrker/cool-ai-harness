import { api } from "./client"
import type { MacroTool, MacroToolCreate, ToolCatalogItem } from "./types"

export const constructorApi = {
  tools: () => api.get<ToolCatalogItem[]>("/api/agent-constructor/tools"),
  macros: () => api.get<MacroTool[]>("/api/agent-constructor/macros"),
  createMacro: (body: MacroToolCreate) =>
    api.post<MacroTool>("/api/agent-constructor/macros", body),
  updateMacro: (id: number, body: Partial<MacroToolCreate> & { is_active?: boolean }) =>
    api.patch<MacroTool>(`/api/agent-constructor/macros/${id}`, body),
  deleteMacro: (id: number) =>
    api.delete<{ deleted: number }>(`/api/agent-constructor/macros/${id}`),
}
