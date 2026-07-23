import { api } from "./client"
import type { SystemPromptResponse, SystemPromptUpdate } from "./types"

export const settingsApi = {
  getSystemPrompt: () => api.get<SystemPromptResponse>("/api/settings/system-prompt"),
  updateSystemPrompt: (body: SystemPromptUpdate) =>
    api.put<SystemPromptResponse>("/api/settings/system-prompt", body),
}
