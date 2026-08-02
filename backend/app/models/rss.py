"""RSS subscription and entry models (Фаза 3b §6 — RSS / News Aggregator).

An ``RssSubscription`` is a user-managed feed source: the URL, display
metadata, fetch cadence and category. An ``RssEntry`` is one parsed item from
a feed — deduplicated by GUID and content hash so re-fetching a feed never
produces duplicates.
"""

from __future__ import annotations

from datetime import datetime

from sqlalchemy import Column, Text
from sqlmodel import Field

from app.models.base import TimestampMixin, _utcnow


class RssSubscription(TimestampMixin, table=True):
    """A user's RSS/Atom feed subscription."""

    __tablename__ = "rss_subscriptions"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    url: str = Field(index=True)
    # Feed metadata discovered on first fetch.
    title: str | None = None
    site_url: str | None = None
    # User-assigned category for grouping (e.g. "ai", "news", "dev").
    category: str | None = Field(default=None, index=True)
    # How often the maintenance sweep fetches this feed.
    fetch_interval_minutes: int = 60
    enabled: bool = Field(default=True, index=True)
    last_fetched_at: datetime | None = None
    last_error: str | None = Field(default=None, sa_column=Column(Text))
    # Number of entries stored for this subscription.
    entry_count: int = 0


class RssEntry(TimestampMixin, table=True):
    """One item parsed from an RSS/Atom feed."""

    __tablename__ = "rss_entries"

    id: int | None = Field(default=None, primary_key=True)
    subscription_id: int = Field(foreign_key="rss_subscriptions.id", index=True)
    # Feed-provided unique id (or generated from link hash).
    guid: str = Field(index=True)
    title: str | None = None
    link: str | None = None
    author: str | None = None
    summary: str | None = Field(default=None, sa_column=Column(Text))
    published_at: datetime | None = Field(default=None, index=True)
    # SHA-256 of the entry content — used for cross-feed deduplication.
    content_hash: str | None = Field(default=None, index=True)
    is_read: bool = Field(default=False, index=True)
    fetched_at: datetime = Field(default_factory=_utcnow, nullable=False)
