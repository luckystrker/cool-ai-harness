import { idempotencyKey, sdk } from "./sdk"
import { toProfile } from "./mappers"
import type { ProfileCreate, ProfileUpdate } from "./types"

export const profilesApi = {
  list: async (includeInactive = false) =>
    (await sdk.profilesList({ includeInactive })).map(toProfile),

  get: async (id: number) => toProfile(await sdk.profilesGet({ id })),

  create: async (body: ProfileCreate) =>
    toProfile(
      await sdk.profilesCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        slug: body.slug,
        description: body.description ?? null,
        systemPrompt: body.system_prompt ?? null,
        model: body.model ?? null,
        toolNames: (body.tool_names as unknown as import("./generated/cool_protocol").JsonValue) ?? null,
        skillNames: (body.skill_names as unknown as import("./generated/cool_protocol").JsonValue) ?? null,
        settings: (body.settings as unknown as import("./generated/cool_protocol").JsonValue) ?? null,
        avatarColor: body.avatar_color ?? null,
        isBuiltin: false,
        isActive: true,
        isShared: body.is_shared ?? false,
      })
    ),

  update: async (id: number, body: ProfileUpdate) =>
    toProfile(
      await sdk.profilesUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        name: body.name ?? null,
        slug: body.slug ?? null,
        description: body.description ?? null,
        systemPrompt: body.system_prompt ?? null,
        model: body.model ?? null,
        toolNames: (body.tool_names as unknown as import("./generated/cool_protocol").JsonValue) ?? null,
        skillNames: (body.skill_names as unknown as import("./generated/cool_protocol").JsonValue) ?? null,
        settings: (body.settings as unknown as import("./generated/cool_protocol").JsonValue) ?? null,
        avatarColor: body.avatar_color ?? null,
        isActive: body.is_active ?? null,
        isShared: body.is_shared ?? null,
      })
    ),

  delete: async (id: number) => sdk.profilesDelete({ idempotencyKey: idempotencyKey(), id }),

  seed: async () => sdk.profilesSeed({ idempotencyKey: idempotencyKey() }),

  clone: async (id: number) =>
    toProfile(await sdk.profilesClone({ idempotencyKey: idempotencyKey(), id })),

  playground: async (id: number, body: { title?: string; initial_prompt?: string } = {}) => {
    const result = await sdk.profilesPlayground({
      idempotencyKey: idempotencyKey(),
      id,
      title: body.title ?? null,
      initialPrompt: body.initial_prompt ?? null,
    })
    return { conversation_id: result.conversationId }
  },
}
