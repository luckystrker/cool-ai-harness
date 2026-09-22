import { idempotencyKey, sdk } from "./sdk"
import { toSkill } from "./mappers"
import type { SkillCreateRequest, SkillCreateResponse, SkillListResponse } from "./types"

export const skillsApi = {
  /** List all available skills, optionally filtered by source. */
  list: async (source?: string): Promise<SkillListResponse> => {
    const result = await sdk.skillsList({ source: source ?? null })
    return { skills: result.skills.map(toSkill) }
  },

  /** Create a new skill. */
  create: async (body: SkillCreateRequest): Promise<SkillCreateResponse> =>
    sdk.skillsCreate({
      idempotencyKey: idempotencyKey(),
      name: body.name,
      description: body.description ?? "",
      tags: body.tags ?? [],
      tools: body.tools ?? [],
      body: body.body,
      scope: body.scope ?? "user",
    }),

  /** Delete a user-created skill. */
  delete: async (name: string): Promise<void> => {
    await sdk.skillsDelete({ idempotencyKey: idempotencyKey(), name })
  },
}
