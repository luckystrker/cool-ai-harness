"""RSS tools: let the agent manage feed subscriptions (Фаза 3b §6).

Registered tools: ``rss_subscribe``, ``rss_unsubscribe``, ``rss_list``,
``rss_fetch``. These allow the agent to set up news monitoring on the user's
behalf ("subscribe to the OpenAI blog RSS") and pull fresh entries on demand.
"""

from __future__ import annotations

from typing import Any

from pydantic import Field

from app.tools.base import ToolArgs, ToolResult, register_tool


class RssSubscribeArgs(ToolArgs):
    """Arguments for rss_subscribe."""

    url: str = Field(description="RSS/Atom feed URL to subscribe to.")
    category: str | None = Field(
        default=None, description="Optional category for grouping (e.g. 'ai', 'news')."
    )


class RssUnsubscribeArgs(ToolArgs):
    """Arguments for rss_unsubscribe."""

    subscription_id: int = Field(description="Id of the subscription to remove.")


class RssListArgs(ToolArgs):
    """Arguments for rss_list."""

    category: str | None = Field(
        default=None, description="Filter by category (omit to list all)."
    )
    include_entries: bool = Field(
        default=True, description="Include the 5 most recent entries per subscription."
    )


class RssFetchArgs(ToolArgs):
    """Arguments for rss_fetch."""

    subscription_id: int | None = Field(
        default=None,
        description="Id of a specific subscription to fetch. Omit to fetch all due feeds.",
    )


def _sub_summary(sub: Any, entries: list | None = None) -> dict:
    result = {
        "id": sub.id,
        "url": sub.url,
        "title": sub.title,
        "site_url": sub.site_url,
        "category": sub.category,
        "enabled": sub.enabled,
        "entry_count": sub.entry_count,
        "last_fetched_at": sub.last_fetched_at.isoformat() if sub.last_fetched_at else None,
        "last_error": sub.last_error,
    }
    if entries is not None:
        result["recent_entries"] = entries
    return result


def _entry_summary(entry: Any) -> dict:
    return {
        "id": entry.id,
        "title": entry.title,
        "link": entry.link,
        "author": entry.author,
        "published_at": entry.published_at.isoformat() if entry.published_at else None,
        "is_read": entry.is_read,
    }


async def _rss_subscribe(url: str, category: str | None = None) -> ToolResult:
    """Subscribe to an RSS/Atom feed."""
    from sqlmodel import Session

    from app.agent.service import get_or_create_default_user
    from app.core.db import engine
    from app.rss.service import subscribe

    with Session(engine) as session:
        user = get_or_create_default_user(session)
        assert user.id is not None
        try:
            sub = subscribe(session, user_id=user.id, url=url, category=category)
        except ValueError as exc:
            return ToolResult.err(str(exc))
        return ToolResult.ok({"subscribed": _sub_summary(sub)}, subscription_id=sub.id)


async def _rss_unsubscribe(subscription_id: int) -> ToolResult:
    """Remove an RSS subscription and its entries."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.rss.service import unsubscribe

    with Session(engine) as session:
        if not unsubscribe(session, subscription_id):
            return ToolResult.err(f"Subscription {subscription_id} not found.")
        return ToolResult.ok({"unsubscribed": subscription_id})


async def _rss_list(category: str | None = None, include_entries: bool = True) -> ToolResult:
    """List RSS subscriptions, optionally with recent entries."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.rss.service import list_entries, list_subscriptions

    with Session(engine) as session:
        subs = list_subscriptions(session, category=category)
        results = []
        for sub in subs:
            entries = None
            if include_entries and sub.id is not None:
                recent = list_entries(session, subscription_id=sub.id, limit=5)
                entries = [_entry_summary(e) for e in recent]
            results.append(_sub_summary(sub, entries))
        return ToolResult.ok(results, count=len(results))


async def _rss_fetch(subscription_id: int | None = None) -> ToolResult:
    """Fetch new entries from one or all due subscriptions."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.rss.service import fetch_all_due, fetch_feed, get_subscription

    with Session(engine) as session:
        if subscription_id is not None:
            sub = get_subscription(session, subscription_id)
            if sub is None:
                return ToolResult.err(f"Subscription {subscription_id} not found.")
            count = fetch_feed(session, sub)
            return ToolResult.ok(
                {"subscription_id": subscription_id, "new_entries": count}
            )
        stats = fetch_all_due(session)
        return ToolResult.ok(stats)


def register_rss_tools() -> None:
    """Register RSS management tools. Idempotent."""
    from app.security.capabilities import Capability

    register_tool(
        name="rss_subscribe",
        description=(
            "Subscribe to an RSS or Atom feed URL. The feed will be fetched "
            "periodically and new entries stored for review."
        ),
        args_model=RssSubscribeArgs,
        func=_rss_subscribe,
        capabilities=frozenset({Capability.NETWORK}),
    )
    register_tool(
        name="rss_unsubscribe",
        description="Remove an RSS subscription and all its stored entries.",
        args_model=RssUnsubscribeArgs,
        func=_rss_unsubscribe,
    )
    register_tool(
        name="rss_list",
        description=(
            "List RSS subscriptions with their recent entries. Use to check "
            "what feeds the user follows and see the latest headlines."
        ),
        args_model=RssListArgs,
        func=_rss_list,
        capabilities=frozenset({Capability.READ}),
    )
    register_tool(
        name="rss_fetch",
        description=(
            "Fetch new entries from a specific subscription (by id) or all "
            "due subscriptions. Returns the count of new entries found."
        ),
        args_model=RssFetchArgs,
        func=_rss_fetch,
        capabilities=frozenset({Capability.NETWORK}),
    )
