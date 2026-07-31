import { api } from "./client"
import type { WikiArticle } from "./types"

export const wikiApi = {
  list: (params?: { category?: string; tag?: string; limit?: number; offset?: number }) => {
    const qs = new URLSearchParams()
    if (params?.category) qs.set("category", params.category)
    if (params?.tag) qs.set("tag", params.tag)
    if (params?.limit) qs.set("limit", String(params.limit))
    if (params?.offset) qs.set("offset", String(params.offset))
    const query = qs.toString()
    return api.get<WikiArticle[]>(`/wiki${query ? `?${query}` : ""}`)
  },

  search: (q: string, limit = 20) =>
    api.get<WikiArticle[]>(`/wiki/search?q=${encodeURIComponent(q)}&limit=${limit}`),

  get: (id: number) => api.get<WikiArticle>(`/wiki/${id}`),

  create: (body: { title: string; content: string; category?: string; tags?: string[] }) =>
    api.post<WikiArticle>("/wiki", body),

  update: (id: number, body: Partial<{ title: string; content: string; category: string; tags: string[]; is_pinned: boolean; is_archived: boolean }>) =>
    api.patch<WikiArticle>(`/wiki/${id}`, body),

  delete: (id: number) => api.delete(`/wiki/${id}`),

  categories: () => api.get<string[]>("/wiki/categories"),

  stats: () => api.get<{ total_articles: number; by_category: Record<string, number> }>("/wiki/stats"),

  promote: (body: { memory_item_id: number; title: string; content: string; category?: string; tags?: string[] }) =>
    api.post<WikiArticle>("/wiki/promote", body),
}
