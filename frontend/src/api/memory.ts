import { idempotencyKey, sdk } from "./sdk"
import {
  toEntity,
  toEpisode,
  toMemoryExplain,
  toMemoryItem,
  toMemoryStats,
} from "./mappers"
import type {
  EntityCreate,
  EntityUpdate,
  MemoryCreate,
  MemoryExtractRequest,
  MemoryExtractResponse,
  MemoryUpdate,
} from "./types"
import type { JsonValue } from "./generated/cool_protocol"

export const memoryApi = {
  // --- Memories ---
  list: async (params?: {
    memory_type?: string
    scope?: string
    status?: string
    limit?: number
    offset?: number
  }) =>
    (
      await sdk.memoryList({
        memoryType: params?.memory_type ?? null,
        scope: params?.scope ?? null,
        status: params?.status ?? "active",
        conversationId: null,
        pinned: null,
        limit: params?.limit ?? 50,
        offset: params?.offset ?? 0,
      })
    ).map(toMemoryItem),
  get: async (id: number) => toMemoryItem(await sdk.memoryGet({ id })),
  create: async (body: MemoryCreate) =>
    toMemoryItem(
      await sdk.memoryCreate({
        idempotencyKey: idempotencyKey(),
        scope: body.scope ?? null,
        agentId: body.agent_id ?? null,
        conversationId: null,
        memoryType: body.memory_type ?? null,
        content: body.content,
        structured: (body.structured as unknown as JsonValue) ?? null,
        tags: (body.tags as unknown as JsonValue) ?? null,
        importance: body.importance ?? null,
        confidence: body.confidence ?? null,
        source: null,
        status: null,
        confirmed: false,
        supersedesId: null,
        ttlDays: body.ttl_days ?? null,
        validFrom: null,
        validTo: null,
        pinned: false,
      })
    ),
  update: async (id: number, body: MemoryUpdate) =>
    toMemoryItem(
      await sdk.memoryUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        content: body.content ?? null,
        memoryType: body.memory_type ?? null,
        scope: body.scope ?? null,
        agentId: null,
        importance: body.importance ?? null,
        confidence: body.confidence ?? null,
        status: body.status ?? null,
        tags: (body.tags as unknown as JsonValue) ?? null,
        structured: (body.structured as unknown as JsonValue) ?? null,
        ttlDays: body.ttl_days ?? null,
        validTo: body.valid_to ?? null,
        pinned: body.pinned ?? null,
      })
    ),
  delete: async (id: number, hard = false) => {
    await sdk.memoryDelete({ idempotencyKey: idempotencyKey(), id, hard })
  },

  // --- Confirmation workflow ---
  listPending: async () =>
    (await sdk.memoryPending({ limit: 100, offset: 0 })).map(toMemoryItem),
  confirm: async (id: number) =>
    toMemoryItem(await sdk.memoryConfirm({ idempotencyKey: idempotencyKey(), id })),
  reject: async (id: number) => {
    await sdk.memoryReject({ idempotencyKey: idempotencyKey(), id })
  },
  pin: async (id: number, pinned: boolean) =>
    toMemoryItem(await sdk.memoryPin({ idempotencyKey: idempotencyKey(), id, pinned })),

  // --- Explainability ---
  explain: async (id: number) => toMemoryExplain(await sdk.memoryExplain({ id })),

  // --- Export (triggers a browser download) ---
  exportMemories: async (
    format: "json" | "markdown" = "json",
    includeArchived = false
  ): Promise<void> => {
    const qs = new URLSearchParams({ format })
    if (includeArchived) qs.set("include_archived", "true")
    const resp = await fetch(`/api/memory/export?${qs}`)
    if (!resp.ok) throw new Error(`Export failed: ${resp.status}`)
    const blob = await resp.blob()
    const url = URL.createObjectURL(blob)
    const a = document.createElement("a")
    a.href = url
    a.download = format === "json" ? "memories.json" : "memories.md"
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)
  },

  // --- Episodes ---
  listEpisodes: async (params?: { agent_id?: number; limit?: number }) =>
    (
      await sdk.memoryEpisodes({
        agentId: params?.agent_id ?? null,
        limit: params?.limit ?? 20,
      })
    ).map(toEpisode),

  // --- Stats ---
  stats: async () => toMemoryStats(await sdk.memoryStats({})),

  // --- Extraction ---
  extract: async (
    body: MemoryExtractRequest
  ): Promise<MemoryExtractResponse> => {
    const result = await sdk.memoryExtract({
      idempotencyKey: idempotencyKey(),
      conversationId: body.conversation_id,
    })
    return {
      status: result.status,
      stored_count: result.storedCount,
      detail: result.detail ?? null,
    }
  },
}

export const entitiesApi = {
  list: async (params?: { entity_type?: string; query?: string; limit?: number }) =>
    (
      await sdk.entitiesList({
        entityType: params?.entity_type ?? null,
        query: params?.query ?? null,
        limit: params?.limit ?? 100,
      })
    ).map(toEntity),
  get: async (id: number) => toEntity(await sdk.entitiesGet({ id })),
  create: async (body: EntityCreate) =>
    toEntity(
      await sdk.entitiesCreate({
        idempotencyKey: idempotencyKey(),
        name: body.name,
        entityType: body.entity_type ?? "concept",
        aliases: (body.aliases as unknown as JsonValue) ?? null,
        attributes: (body.attributes as unknown as JsonValue) ?? null,
        description: body.description ?? null,
      })
    ),
  update: async (id: number, body: EntityUpdate) =>
    toEntity(
      await sdk.entitiesUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        name: body.name ?? null,
        entityType: body.entity_type ?? null,
        aliases: (body.aliases as unknown as JsonValue) ?? null,
        attributes: (body.attributes as unknown as JsonValue) ?? null,
        description: body.description ?? null,
      })
    ),
  delete: async (id: number) => {
    await sdk.entitiesDelete({ idempotencyKey: idempotencyKey(), id })
  },
}
