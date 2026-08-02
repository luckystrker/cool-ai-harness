"""Webhook routes: inbound receiver + management CRUD (Фаза 3b §7).

Two routers:
- ``public_router`` (no auth): ``POST /api/webhooks/{hook_id}`` — external systems
- ``router`` (auth): management CRUD at ``/api/webhooks``
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel
from sqlmodel import Session

from app.agent.service import get_or_create_default_user
from app.core.db import get_session
from app.models.webhook import WebhookEndpoint, WebhookEvent
from app.webhooks.service import (
    create_endpoint,
    delete_endpoint,
    get_endpoint,
    get_endpoint_by_hook_id,
    get_event,
    list_endpoints,
    list_events,
    process_event,
    replay_event,
    update_endpoint,
    verify_signature,
)

# --- Management router (authenticated) ------------------------------------

router = APIRouter(prefix="/webhooks", tags=["webhooks"])


# --- Schemas --------------------------------------------------------------


class EndpointCreate(BaseModel):
    name: str
    source_type: str = "custom"
    event_filter: list[str] | None = None
    task_id: int | None = None
    prompt_template: str | None = None
    enabled: bool = True


class EndpointUpdate(BaseModel):
    name: str | None = None
    source_type: str | None = None
    event_filter: list[str] | None = None
    task_id: int | None = None
    prompt_template: str | None = None
    enabled: bool | None = None


class EndpointOut(BaseModel):
    id: int
    user_id: int
    name: str
    hook_id: str
    secret: str
    source_type: str
    event_filter: list[str] | None = None
    task_id: int | None = None
    prompt_template: str | None = None
    enabled: bool
    created_at: datetime
    updated_at: datetime
    # Derived: the full inbound URL path.
    url_path: str = ""


class EventOut(BaseModel):
    id: int
    endpoint_id: int
    event_type: str | None = None
    payload: dict[str, Any] | None = None
    signature_valid: bool
    status: str
    task_run_id: int | None = None
    error: str | None = None
    received_at: datetime
    created_at: datetime


# --- Mappers --------------------------------------------------------------


def _endpoint_to_out(ep: WebhookEndpoint) -> EndpointOut:
    data = ep.model_dump()
    data["url_path"] = f"/api/webhooks/inbound/{ep.hook_id}"
    return EndpointOut(**data)


def _event_to_out(ev: WebhookEvent) -> EventOut:
    return EventOut(**ev.model_dump())


# --- Management endpoints -------------------------------------------------


@router.get("", response_model=list[EndpointOut])
def list_eps(session: Session = Depends(get_session)):
    """List webhook endpoints (newest first)."""
    return [_endpoint_to_out(ep) for ep in list_endpoints(session)]


@router.post("", response_model=EndpointOut, status_code=201)
def create_ep(body: EndpointCreate, session: Session = Depends(get_session)):
    """Create a webhook endpoint."""
    user = get_or_create_default_user(session)
    assert user.id is not None
    try:
        ep = create_endpoint(
            session,
            user_id=user.id,
            name=body.name,
            source_type=body.source_type,
            event_filter=body.event_filter,
            task_id=body.task_id,
            prompt_template=body.prompt_template,
            enabled=body.enabled,
        )
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    return _endpoint_to_out(ep)


@router.get("/{endpoint_id}", response_model=EndpointOut)
def get_ep(endpoint_id: int, session: Session = Depends(get_session)):
    """Get one endpoint."""
    ep = get_endpoint(session, endpoint_id)
    if ep is None:
        raise HTTPException(status_code=404, detail="Endpoint not found")
    return _endpoint_to_out(ep)


@router.put("/{endpoint_id}", response_model=EndpointOut)
def update_ep(endpoint_id: int, body: EndpointUpdate, session: Session = Depends(get_session)):
    """Update an endpoint."""
    fields = body.model_dump(exclude_unset=True)
    if not fields:
        raise HTTPException(status_code=422, detail="No fields to update")
    try:
        ep = update_endpoint(session, endpoint_id, **fields)
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    if ep is None:
        raise HTTPException(status_code=404, detail="Endpoint not found")
    return _endpoint_to_out(ep)


@router.delete("/{endpoint_id}", status_code=204)
def delete_ep(endpoint_id: int, session: Session = Depends(get_session)):
    """Delete an endpoint and its event history."""
    if not delete_endpoint(session, endpoint_id):
        raise HTTPException(status_code=404, detail="Endpoint not found")


@router.get("/{endpoint_id}/events", response_model=list[EventOut])
def ep_events(
    endpoint_id: int,
    status: str | None = Query(default=None),
    limit: int = Query(default=50, ge=1, le=200),
    session: Session = Depends(get_session),
):
    """Event history for one endpoint."""
    ep = get_endpoint(session, endpoint_id)
    if ep is None:
        raise HTTPException(status_code=404, detail="Endpoint not found")
    events = list_events(session, endpoint_id=endpoint_id, status=status, limit=limit)
    return [_event_to_out(e) for e in events]


@router.post("/{endpoint_id}/events/{event_id}/replay", response_model=EventOut)
def replay_ep_event(
    endpoint_id: int, event_id: int, session: Session = Depends(get_session)
):
    """Replay a stored event (re-dispatches it)."""
    event = get_event(session, event_id)
    if event is None or event.endpoint_id != endpoint_id:
        raise HTTPException(status_code=404, detail="Event not found")
    new_event = replay_event(session, event_id)
    if new_event is None:
        raise HTTPException(status_code=500, detail="Replay failed")
    return _event_to_out(new_event)


# --- Public inbound router (no auth) --------------------------------------

public_router = APIRouter(prefix="/webhooks/inbound", tags=["webhooks-public"])


@public_router.post("/{hook_id}")
async def receive_webhook(hook_id: str, request: Request, session: Session = Depends(get_session)):
    """Receive an inbound webhook event from an external system.

    This endpoint is public (no auth token required) — security is provided
    by the HMAC signature and the unguessable hook_id UUID.
    """
    ep = get_endpoint_by_hook_id(session, hook_id)
    if ep is None:
        raise HTTPException(status_code=404, detail="Unknown webhook endpoint")
    if not ep.enabled:
        raise HTTPException(status_code=410, detail="Webhook endpoint is disabled")

    body = await request.body()

    # Extract signature from common header names.
    signature = (
        request.headers.get("x-hub-signature-256")
        or request.headers.get("x-signature")
        or request.headers.get("x-webhook-signature")
    )
    sig_valid = verify_signature(ep.secret, body, signature)

    # Parse payload.
    try:
        import json

        payload = json.loads(body) if body else {}
    except Exception:
        payload = {"raw": body.decode("utf-8", errors="replace")[:5000]}

    # Collect relevant headers for event-type extraction.
    headers = {k.lower(): v for k, v in request.headers.items()}

    event = process_event(
        session,
        ep,
        payload=payload,
        signature_valid=sig_valid,
        headers=headers,
    )
    return {
        "event_id": event.id,
        "status": event.status,
        "event_type": event.event_type,
    }
