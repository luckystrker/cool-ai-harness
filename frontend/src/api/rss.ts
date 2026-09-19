import { api } from "./client"
import { idempotencyKey, sdk } from "./sdk"
import { toRssEntry, toRssSubscription } from "./mappers"

export const rssApi = {
  // --- Subscriptions ---
  listSubscriptions: async (params?: { category?: string; enabled?: boolean }) =>
    (
      await sdk.rssSubscriptionsList({
        category: params?.category ?? null,
        enabled: params?.enabled ?? null,
      })
    ).map(toRssSubscription),
  subscribe: async (body: {
    url: string
    category?: string
    fetch_interval_minutes?: number
  }) =>
    toRssSubscription(
      await sdk.rssSubscribe({
        idempotencyKey: idempotencyKey(),
        url: body.url,
        title: null,
        siteUrl: null,
        category: body.category ?? null,
        fetchIntervalMinutes: body.fetch_interval_minutes ?? null,
        enabled: null,
      })
    ),
  unsubscribe: async (id: number) => {
    await sdk.rssUnsubscribe({ idempotencyKey: idempotencyKey(), id })
  },
  fetchNow: (id: number) =>
    api.post<{ subscription_id: number; new_entries: number }>(
      `/api/rss/subscriptions/${id}/fetch`
    ),

  // --- Entries ---
  listEntries: async (
    subId: number,
    params?: { unread_only?: boolean; limit?: number }
  ) =>
    (
      await sdk.rssEntriesList({
        subscriptionId: subId,
        unreadOnly: params?.unread_only ?? false,
        limit: params?.limit ?? 50,
      })
    ).map(toRssEntry),
  allEntries: async (params?: { unread_only?: boolean; limit?: number }) =>
    (
      await sdk.rssEntriesAll({
        unreadOnly: params?.unread_only ?? false,
        limit: params?.limit ?? 50,
      })
    ).map(toRssEntry),
  markRead: async (entryId: number, isRead = true) =>
    toRssEntry(
      await sdk.rssEntryRead({ idempotencyKey: idempotencyKey(), id: entryId, isRead })
    ),
}
