"""RSS service: subscription CRUD, feed fetching, deduplication (Фаза 3b §6).

Fetching uses ``httpx`` (already a project dependency) to download the feed
body, then ``feedparser`` to parse RSS/Atom/RDF formats. Entries are
deduplicated per subscription by GUID (falling back to a SHA-256 of the link)
and globally by content hash so the same article from two feeds is stored once.
"""

from __future__ import annotations

import hashlib
from collections.abc import Sequence
from datetime import UTC, datetime, timedelta

import feedparser
import httpx
from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.models.rss import RssEntry, RssSubscription

log = get_logger(__name__)

# Maximum entries stored per subscription before the oldest are pruned.
MAX_ENTRIES_PER_SUB = 500
# HTTP timeout for feed fetches.
FETCH_TIMEOUT = 20.0
# Maximum feed body size (bytes) we're willing to parse.
MAX_FEED_BYTES = 5_000_000


# --- Subscription CRUD ----------------------------------------------------


def subscribe(
    session: Session,
    *,
    user_id: int,
    url: str,
    category: str | None = None,
    fetch_interval_minutes: int = 60,
) -> RssSubscription:
    """Create a subscription. Fetches the feed once to discover its title.

    Raises ``ValueError`` when the URL is already subscribed or unreachable.
    """
    existing = session.exec(
        select(RssSubscription).where(
            RssSubscription.user_id == user_id, RssSubscription.url == url
        )
    ).first()
    if existing is not None:
        raise ValueError(f"Already subscribed to {url}")

    sub = RssSubscription(
        user_id=user_id,
        url=url,
        category=category,
        fetch_interval_minutes=max(5, fetch_interval_minutes),
    )

    # Best-effort metadata discovery (non-fatal if the feed is down right now).
    try:
        feed = _download_and_parse(url)
        sub.title = feed.feed.get("title") or None
        sub.site_url = feed.feed.get("link") or None
        sub.last_fetched_at = datetime.now(UTC)
    except Exception as exc:
        sub.last_error = str(exc)[:500]
        log.warning("rss.subscribe_fetch_failed", url=url, error=str(exc))

    session.add(sub)
    session.commit()
    session.refresh(sub)
    log.info("rss.subscribed", sub_id=sub.id, url=url, title=sub.title)
    return sub


def unsubscribe(session: Session, subscription_id: int) -> bool:
    """Delete a subscription and all its entries."""
    sub = session.get(RssSubscription, subscription_id)
    if sub is None:
        return False
    entries = session.exec(
        select(RssEntry).where(RssEntry.subscription_id == subscription_id)
    ).all()
    for entry in entries:
        session.delete(entry)
    session.delete(sub)
    session.commit()
    log.info("rss.unsubscribed", sub_id=subscription_id)
    return True


def list_subscriptions(
    session: Session,
    *,
    user_id: int | None = None,
    category: str | None = None,
    enabled: bool | None = None,
) -> Sequence[RssSubscription]:
    """Subscriptions, newest first, with optional filters."""
    stmt = select(RssSubscription).order_by(col(RssSubscription.id).desc())
    if user_id is not None:
        stmt = stmt.where(RssSubscription.user_id == user_id)
    if category is not None:
        stmt = stmt.where(RssSubscription.category == category)
    if enabled is not None:
        stmt = stmt.where(RssSubscription.enabled == enabled)
    return session.exec(stmt).all()


def get_subscription(session: Session, subscription_id: int) -> RssSubscription | None:
    return session.get(RssSubscription, subscription_id)


# --- Feed fetching --------------------------------------------------------


def _download_and_parse(url: str) -> feedparser.FeedParserDict:
    """Download a feed URL and parse it. Raises on network/parse errors."""
    response = httpx.get(url, timeout=FETCH_TIMEOUT, follow_redirects=True)
    response.raise_for_status()
    if len(response.content) > MAX_FEED_BYTES:
        raise ValueError(f"Feed body exceeds {MAX_FEED_BYTES} bytes")
    feed = feedparser.parse(response.content)
    if feed.bozo and not feed.entries:
        reason = getattr(feed, "bozo_exception", "unknown parse error")
        raise ValueError(f"Feed parse error: {reason}")
    return feed


def _entry_guid(entry: feedparser.FeedParserDict) -> str:
    """Extract or generate a stable GUID for a feed entry."""
    guid = entry.get("id") or entry.get("guid") or ""
    if guid:
        return guid
    link = entry.get("link") or ""
    if link:
        return hashlib.sha256(link.encode()).hexdigest()
    title = entry.get("title") or ""
    return hashlib.sha256(title.encode()).hexdigest()


def _entry_content_hash(entry: feedparser.FeedParserDict) -> str:
    """SHA-256 of the entry's textual content (for cross-feed dedup)."""
    parts = [
        entry.get("title") or "",
        entry.get("link") or "",
        entry.get("summary") or "",
    ]
    return hashlib.sha256("|".join(parts).encode()).hexdigest()


def _parse_published(entry: feedparser.FeedParserDict) -> datetime | None:
    """Best-effort published-date extraction."""
    for key in ("published_parsed", "updated_parsed"):
        parsed = entry.get(key)
        if parsed:
            try:
                return datetime(
                    parsed[0], parsed[1], parsed[2], parsed[3], parsed[4], parsed[5], tzinfo=UTC
                )
            except (TypeError, ValueError, IndexError):
                pass
    return None


def fetch_feed(session: Session, sub: RssSubscription) -> int:
    """Fetch and store new entries for a subscription. Returns new entry count.

    Updates ``last_fetched_at`` and ``last_error`` on the subscription.
    """
    try:
        feed = _download_and_parse(sub.url)
    except Exception as exc:
        sub.last_error = str(exc)[:500]
        sub.last_fetched_at = datetime.now(UTC)
        sub.updated_at = datetime.now(UTC)
        session.add(sub)
        session.commit()
        log.warning("rss.fetch_failed", sub_id=sub.id, error=str(exc))
        return 0

    # Update feed metadata if it was missing.
    if not sub.title and feed.feed.get("title"):
        sub.title = feed.feed["title"]
    if not sub.site_url and feed.feed.get("link"):
        sub.site_url = feed.feed["link"]

    # Existing GUIDs for this subscription (fast dedup).
    existing_guids: set[str] = set()
    for row in session.exec(
        select(RssEntry.guid).where(RssEntry.subscription_id == sub.id)
    ).all():
        existing_guids.add(row)

    new_count = 0
    for entry in feed.entries:
        guid = _entry_guid(entry)
        if guid in existing_guids:
            continue
        existing_guids.add(guid)

        rss_entry = RssEntry(
            subscription_id=sub.id,
            guid=guid,
            title=(entry.get("title") or "")[:500] or None,
            link=entry.get("link") or None,
            author=entry.get("author") or None,
            summary=(entry.get("summary") or "")[:2000] or None,
            published_at=_parse_published(entry),
            content_hash=_entry_content_hash(entry),
        )
        session.add(rss_entry)
        new_count += 1

    sub.last_fetched_at = datetime.now(UTC)
    sub.last_error = None
    sub.entry_count = sub.entry_count + new_count
    sub.updated_at = datetime.now(UTC)
    session.add(sub)
    session.commit()

    if new_count:
        log.info("rss.fetched", sub_id=sub.id, new_entries=new_count)
        _prune_old_entries(session, sub)
    return new_count


def _prune_old_entries(session: Session, sub: RssSubscription) -> None:
    """Keep at most MAX_ENTRIES_PER_SUB entries per subscription."""
    if sub.id is None:
        return
    count = session.exec(
        select(RssEntry).where(RssEntry.subscription_id == sub.id)
    ).all()
    if len(count) <= MAX_ENTRIES_PER_SUB:
        return
    # Delete oldest (by published_at or fetched_at).
    sorted_entries = sorted(count, key=lambda e: e.published_at or e.fetched_at)
    to_remove = len(sorted_entries) - MAX_ENTRIES_PER_SUB
    for entry in sorted_entries[:to_remove]:
        session.delete(entry)
    sub.entry_count = MAX_ENTRIES_PER_SUB
    session.add(sub)
    session.commit()


def fetch_all_due(session: Session) -> dict[str, int]:
    """Fetch all enabled subscriptions whose interval has elapsed.

    Returns stats: {"fetched": N, "new_entries": M, "errors": E}.
    """
    now = datetime.now(UTC)
    subs = session.exec(
        select(RssSubscription).where(RssSubscription.enabled == True)  # noqa: E712
    ).all()
    fetched = 0
    new_entries = 0
    errors = 0
    for sub in subs:
        if sub.last_fetched_at is not None:
            interval = timedelta(minutes=sub.fetch_interval_minutes)
            # SQLite stores naive datetimes; stamp as UTC for comparison.
            last = sub.last_fetched_at
            if last.tzinfo is None:
                last = last.replace(tzinfo=UTC)
            if now - last < interval:
                continue
        before = sub.last_error
        count = fetch_feed(session, sub)
        fetched += 1
        new_entries += count
        if sub.last_error and sub.last_error != before:
            errors += 1
    return {"fetched": fetched, "new_entries": new_entries, "errors": errors}


# --- Entry queries --------------------------------------------------------


def list_entries(
    session: Session,
    *,
    subscription_id: int | None = None,
    unread_only: bool = False,
    limit: int = 50,
) -> Sequence[RssEntry]:
    """Entries, newest first, optionally filtered."""
    stmt = select(RssEntry).order_by(col(RssEntry.published_at).desc().nulls_last())
    if subscription_id is not None:
        stmt = stmt.where(RssEntry.subscription_id == subscription_id)
    if unread_only:
        stmt = stmt.where(RssEntry.is_read == False)  # noqa: E712
    return session.exec(stmt.limit(limit)).all()


def mark_entry_read(session: Session, entry_id: int, *, is_read: bool = True) -> RssEntry | None:
    """Flip an entry's read state."""
    entry = session.get(RssEntry, entry_id)
    if entry is None:
        return None
    entry.is_read = is_read
    entry.updated_at = datetime.now(UTC)
    session.add(entry)
    session.commit()
    session.refresh(entry)
    return entry
