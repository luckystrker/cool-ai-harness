import { api } from "./client"
import type { RssEntry, RssSubscription } from "./types"

export const rssApi = {
  // --- Subscriptions ---
  listSubscriptions: (params?: { category?: string; enabled?: boolean }) => {
    const qs = new URLSearchParams()
    if (params?.category) qs.set("category", params.category)
    if (params?.enabled != null) qs.set("enabled", String(params.enabled))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<RssSubscription[]>(`/api/rss/subscriptions${suffix}`)
  },
  subscribe: (body: { url: string; category?: string; fetch_interval_minutes?: number }) =>
    api.post<RssSubscription>("/api/rss/subscriptions", body),
  unsubscribe: (id: number) => api.delete<void>(`/api/rss/subscriptions/${id}`),
  fetchNow: (id: number) =>
    api.post<{ subscription_id: number; new_entries: number }>(
      `/api/rss/subscriptions/${id}/fetch`
    ),

  // --- Entries ---
  listEntries: (subId: number, params?: { unread_only?: boolean; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.unread_only) qs.set("unread_only", "true")
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<RssEntry[]>(`/api/rss/subscriptions/${subId}/entries${suffix}`)
  },
  allEntries: (params?: { unread_only?: boolean; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.unread_only) qs.set("unread_only", "true")
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const suffix = qs.toString() ? `?${qs}` : ""
    return api.get<RssEntry[]>(`/api/rss/entries${suffix}`)
  },
  markRead: (entryId: number, isRead = true) =>
    api.post<RssEntry>(`/api/rss/entries/${entryId}/read`, { is_read: isRead }),
}
