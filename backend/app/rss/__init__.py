"""RSS / News Aggregator (Фаза 3b §6).

Layout:
- ``service`` — subscription CRUD, feed fetching, entry deduplication, queries
"""

from __future__ import annotations

from app.rss.service import (
    fetch_all_due,
    fetch_feed,
    list_entries,
    list_subscriptions,
    mark_entry_read,
    subscribe,
    unsubscribe,
)

__all__ = [
    "fetch_all_due",
    "fetch_feed",
    "list_entries",
    "list_subscriptions",
    "mark_entry_read",
    "subscribe",
    "unsubscribe",
]
