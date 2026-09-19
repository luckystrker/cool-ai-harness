import { api } from "./client"
import { idempotencyKey, sdk } from "./sdk"
import { toProvider } from "./mappers"
import type { ModelInfo, ModelsPreviewRequest, ProviderCreate, ProviderUpdate } from "./types"
import type { JsonValue } from "./generated/cool_protocol"

export const providersApi = {
  list: async () => (await sdk.providersList({ includeInactive: true })).map(toProvider),

  create: async (body: ProviderCreate) =>
    toProvider(
      await sdk.providersCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        label: body.label ?? null,
        baseUrl: body.base_url ?? null,
        apiKey: body.api_key,
        defaultModel: body.default_model ?? null,
        isActive: true,
        isSubscription: body.is_subscription ?? false,
        isFallback: body.is_fallback ?? false,
        isDefault: body.is_default ?? false,
        chatModels: (body.chat_models as unknown as JsonValue) ?? null,
      })
    ),

  get: async (id: number) => toProvider(await sdk.providersGet({ id })),

  update: async (id: number, body: ProviderUpdate) =>
    toProvider(
      await sdk.providersUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        label: body.label ?? null,
        baseUrl: body.base_url ?? null,
        apiKey: body.api_key ?? null,
        defaultModel: body.default_model ?? null,
        isActive: body.is_active ?? null,
        isFallback: body.is_fallback ?? null,
        isDefault: body.is_default ?? null,
        chatModels: (body.chat_models as unknown as JsonValue) ?? null,
      })
    ),

  delete: async (id: number) =>
    sdk.providersDelete({ idempotencyKey: idempotencyKey(), id }),

  /** Models served by an already-saved provider (live provider probe). */
  listModels: (id: number) =>
    api.get<ModelInfo[]>(`/api/providers/${id}/models`),

  /** Live model-list probe for an unsaved provider (create form). */
  previewModels: (body: ModelsPreviewRequest) =>
    api.post<ModelInfo[]>("/api/providers/models/preview", body),
}
