import { idempotencyKey, sdk } from "./sdk"
import { toWikiArticle, toWikiStats } from "./mappers"
import type { JsonValue } from "./generated/cool_protocol"

export const wikiApi = {
  list: async (params?: { category?: string; tag?: string; limit?: number; offset?: number }) =>
    (
      await sdk.wikiList({
        category: params?.category ?? null,
        tag: params?.tag ?? null,
        archived: false,
        projectKey: null,
        pinned: null,
        search: null,
        limit: params?.limit ?? 100,
        offset: params?.offset ?? 0,
      })
    ).map(toWikiArticle),

  search: async (q: string, limit = 20) =>
    (await sdk.wikiSearch({ query: q, limit })).map(toWikiArticle),

  get: async (id: number) => toWikiArticle(await sdk.wikiGet({ id })),

  create: async (body: { title: string; content: string; category?: string; tags?: string[] }) =>
    toWikiArticle(
      await sdk.wikiCreate({
        idempotencyKey: idempotencyKey(),
        title: body.title,
        content: body.content,
        category: body.category ?? null,
        tags: (body.tags as unknown as JsonValue) ?? null,
        source: null,
        sourceMemoryId: null,
        projectKey: null,
        metadata: null,
      })
    ),

  update: async (
    id: number,
    body: Partial<{
      title: string
      content: string
      category: string
      tags: string[]
      is_pinned: boolean
      is_archived: boolean
    }>
  ) =>
    toWikiArticle(
      await sdk.wikiUpdate({
        idempotencyKey: idempotencyKey(),
        id,
        title: body.title ?? null,
        content: body.content ?? null,
        category: body.category ?? null,
        tags: (body.tags as unknown as JsonValue) ?? null,
        isPinned: body.is_pinned ?? null,
        isArchived: body.is_archived ?? null,
      })
    ),

  delete: async (id: number) => {
    await sdk.wikiDelete({ idempotencyKey: idempotencyKey(), id })
  },

  categories: async () => sdk.wikiCategories({}),

  stats: async () => toWikiStats(await sdk.wikiStats({})),

  promote: async (body: {
    memory_item_id: number
    title: string
    content: string
    category?: string
    tags?: string[]
  }) =>
    toWikiArticle(
      await sdk.wikiPromote({
        idempotencyKey: idempotencyKey(),
        memoryItemId: body.memory_item_id,
        title: body.title,
        content: body.content,
        category: body.category ?? null,
        tags: (body.tags as unknown as JsonValue) ?? null,
      })
    ),
}
