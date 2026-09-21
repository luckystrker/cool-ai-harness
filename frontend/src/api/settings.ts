import { idempotencyKey, sdk } from "./sdk"
import type { SystemPromptResponse, SystemPromptUpdate } from "./types"

function toSystemPromptResponse(record: {
  prompt: string
  isCustom: boolean
  source: string
}): SystemPromptResponse {
  return {
    prompt: record.prompt,
    is_custom: record.isCustom,
    source: record.source === "inline" || record.source === "file" ? record.source : "builtin",
  }
}

export const settingsApi = {
  /** The default system prompt applied to a run that does not supply one. */
  getSystemPrompt: async (): Promise<SystemPromptResponse> =>
    toSystemPromptResponse(await sdk.settingsSystemPrompt({})),
  /** Persist the default prompt; an empty value resets it to the built-in default. */
  updateSystemPrompt: async (body: SystemPromptUpdate): Promise<SystemPromptResponse> =>
    toSystemPromptResponse(
      await sdk.settingsSystemPromptSet({
        idempotencyKey: idempotencyKey(),
        prompt: body.prompt,
      })
    ),
}
