"""RSS routes: subscription CRUD, entries, force-fetch (Фаза 3b §6).

Mounted at ``/api/rss``.
"""

from __future__ import annotations

from datetime import datetime

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel
from sqlmodel import Session

from app.agent.service import get_or_create_default_user
from app.core.db import get_session
from app.models.rss import RssEntry, RssSubscription
from app.rss.service import (
    fetch_feed,
    get_subscription,
    list_entries,
    list_subscriptions,
    mark_entry_read,
    subscribe,
    unsubscribe,
)

router = APIRouter(prefix="/rss", tags=["rss"])


# --- Schemas --------------------------------------------------------------


class SubscribeRequest(BaseModel):
    url: str
    category: str | None = None
    fetch_interval_minutes: int = 60


class SubscriptionOut(BaseModel):
    id: int
    user_id: int
    url: str
    title: str | None = None
    site_url: str | None = None
    category: str | None = None
    fetch_interval_minutes: int
    enabled: bool
    last_fetched_at: datetime | None = None
    last_error: str | None = None
    entry_count: int
    created_at: datetime
    updated_at: datetime


class EntryOut(BaseModel):
    id: int
    subscription_id: int
    guid: str
    title: str | None = None
    link: str | None = None
    author: str | None = None
    summary: str | None = None
    published_at: datetime | None = None
    content_hash: str | None = None
    is_read: bool
    fetched_at: datetime


class ReadRequest(BaseModel):
    is_read: bool = True


# --- Mappers --------------------------------------------------------------


def _sub_to_out(sub: RssSubscription) -> SubscriptionOut:
    return SubscriptionOut(**sub.model_dump())


def _entry_to_out(entry: RssEntry) -> EntryOut:
    return EntryOut(**entry.model_dump())


# --- Subscription routes --------------------------------------------------


@router.get("/subscriptions", response_model=list[SubscriptionOut])
def list_subs(
    category: str | None = Query(default=None),
    enabled: bool | None = Query(default=None),
    session: Session = Depends(get_session),
):
    """List RSS subscriptions (newest first)."""
    subs = list_subscriptions(session, category=category, enabled=enabled)
    return [_sub_to_out(s) for s in subs]


@router.post("/subscriptions", response_model=SubscriptionOut, status_code=201)
def create_sub(body: SubscribeRequest, session: Session = Depends(get_session)):
    """Subscribe to a feed URL."""
    user = get_or_create_default_user(session)
    assert user.id is not None
    try:
        sub = subscribe(
            session,
            user_id=user.id,
            url=body.url,
            category=body.category,
            fetch_interval_minutes=body.fetch_interval_minutes,
        )
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    return _sub_to_out(sub)


@router.delete("/subscriptions/{sub_id}", status_code=204)
def delete_sub(sub_id: int, session: Session = Depends(get_session)):
    """Unsubscribe and delete all entries."""
    if not unsubscribe(session, sub_id):
        raise HTTPException(status_code=404, detail="Subscription not found")


@router.get("/subscriptions/{sub_id}/entries", response_model=list[EntryOut])
def sub_entries(
    sub_id: int,
    unread_only: bool = Query(default=False),
    limit: int = Query(default=50, ge=1, le=200),
    session: Session = Depends(get_session),
):
    """Entries for one subscription (newest first)."""
    sub = get_subscription(session, sub_id)
    if sub is None:
        raise HTTPException(status_code=404, detail="Subscription not found")
    entries = list_entries(session, subscription_id=sub_id, unread_only=unread_only, limit=limit)
    return [_entry_to_out(e) for e in entries]


@router.post("/subscriptions/{sub_id}/fetch")
def force_fetch(sub_id: int, session: Session = Depends(get_session)):
    """Force-fetch a subscription now (ignores interval)."""
    sub = get_subscription(session, sub_id)
    if sub is None:
        raise HTTPException(status_code=404, detail="Subscription not found")
    count = fetch_feed(session, sub)
    return {"subscription_id": sub_id, "new_entries": count}


# --- Entry routes ---------------------------------------------------------


@router.get("/entries", response_model=list[EntryOut])
def all_entries(
    unread_only: bool = Query(default=False),
    limit: int = Query(default=50, ge=1, le=200),
    session: Session = Depends(get_session),
):
    """All entries across subscriptions (inbox-style, newest first)."""
    entries = list_entries(session, unread_only=unread_only, limit=limit)
    return [_entry_to_out(e) for e in entries]


@router.post("/entries/{entry_id}/read", response_model=EntryOut)
def set_entry_read(entry_id: int, body: ReadRequest, session: Session = Depends(get_session)):
    """Mark an entry read/unread."""
    entry = mark_entry_read(session, entry_id, is_read=body.is_read)
    if entry is None:
        raise HTTPException(status_code=404, detail="Entry not found")
    return _entry_to_out(entry)
