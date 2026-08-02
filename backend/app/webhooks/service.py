"""Webhook service: endpoint CRUD, HMAC verification, event dispatch (Фаза 3b §7).

An external system POSTs to ``/api/webhooks/{hook_id}``. The API layer verifies
the HMAC signature and extracts the event type, then hands off to
:func:`process_event` which records the event and either triggers a linked
``ScheduledTask`` or spawns an ad-hoc agent run with the event context.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import secrets
import uuid
from collections.abc import Sequence
from datetime import UTC, datetime
from typing import Any

from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.models.webhook import (
    EVENT_COMPLETED,
    EVENT_FAILED,
    EVENT_PROCESSING,
    EVENT_RECEIVED,
    EVENT_REJECTED,
    SOURCE_TYPES,
    WebhookEndpoint,
    WebhookEvent,
)

log = get_logger(__name__)


# --- Endpoint CRUD --------------------------------------------------------


def create_endpoint(
    session: Session,
    *,
    user_id: int,
    name: str,
    source_type: str = "custom",
    event_filter: list[str] | None = None,
    task_id: int | None = None,
    prompt_template: str | None = None,
    enabled: bool = True,
) -> WebhookEndpoint:
    """Create a webhook endpoint with a generated hook_id and HMAC secret."""
    if source_type not in SOURCE_TYPES:
        raise ValueError(f"Unknown source_type {source_type!r} (expected one of {sorted(SOURCE_TYPES)})")
    endpoint = WebhookEndpoint(
        user_id=user_id,
        name=name,
        hook_id=uuid.uuid4().hex,
        secret=secrets.token_hex(32),
        source_type=source_type,
        event_filter=event_filter,
        task_id=task_id,
        prompt_template=prompt_template,
        enabled=enabled,
    )
    session.add(endpoint)
    session.commit()
    session.refresh(endpoint)
    log.info("webhook.created", endpoint_id=endpoint.id, name=name, source=source_type)
    return endpoint


def get_endpoint(session: Session, endpoint_id: int) -> WebhookEndpoint | None:
    return session.get(WebhookEndpoint, endpoint_id)


def get_endpoint_by_hook_id(session: Session, hook_id: str) -> WebhookEndpoint | None:
    return session.exec(
        select(WebhookEndpoint).where(WebhookEndpoint.hook_id == hook_id)
    ).first()


def list_endpoints(
    session: Session,
    *,
    user_id: int | None = None,
) -> Sequence[WebhookEndpoint]:
    """Endpoints, newest first."""
    stmt = select(WebhookEndpoint).order_by(col(WebhookEndpoint.id).desc())
    if user_id is not None:
        stmt = stmt.where(WebhookEndpoint.user_id == user_id)
    return session.exec(stmt).all()


def update_endpoint(session: Session, endpoint_id: int, **fields: Any) -> WebhookEndpoint | None:
    """Patch an endpoint."""
    endpoint = session.get(WebhookEndpoint, endpoint_id)
    if endpoint is None:
        return None
    if "source_type" in fields and fields["source_type"] not in SOURCE_TYPES:
        raise ValueError(f"Unknown source_type {fields['source_type']!r}")
    allowed = {"name", "source_type", "event_filter", "task_id", "prompt_template", "enabled"}
    for key, value in fields.items():
        if key in allowed:
            setattr(endpoint, key, value)
    endpoint.updated_at = datetime.now(UTC)
    session.add(endpoint)
    session.commit()
    session.refresh(endpoint)
    return endpoint


def delete_endpoint(session: Session, endpoint_id: int) -> bool:
    """Delete an endpoint and its event history."""
    endpoint = session.get(WebhookEndpoint, endpoint_id)
    if endpoint is None:
        return False
    events = session.exec(
        select(WebhookEvent).where(WebhookEvent.endpoint_id == endpoint_id)
    ).all()
    for event in events:
        session.delete(event)
    session.delete(endpoint)
    session.commit()
    log.info("webhook.deleted", endpoint_id=endpoint_id)
    return True


# --- Signature verification -----------------------------------------------


def verify_signature(secret: str, body: bytes, signature: str | None) -> bool:
    """Verify an HMAC-SHA256 signature.

    Supports common header formats:
    - GitHub: ``sha256=<hex>``
    - Slack: bare hex or ``v0=<hex>``
    - Custom: bare hex
    """
    if not signature:
        return False
    # Strip known prefixes.
    sig = signature
    for prefix in ("sha256=", "v0="):
        if sig.startswith(prefix):
            sig = sig[len(prefix):]
            break
    expected = hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, sig)


def compute_signature(secret: str, body: bytes) -> str:
    """Compute the HMAC-SHA256 hex digest for a payload (used in tests/replay)."""
    return hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


# --- Event processing -----------------------------------------------------


def _extract_event_type(source_type: str, headers: dict[str, str], payload: dict) -> str | None:
    """Best-effort event type extraction based on the source."""
    if source_type == "github":
        return headers.get("x-github-event") or payload.get("action")
    if source_type == "slack":
        return payload.get("type") or payload.get("event", {}).get("type")
    if source_type == "notion":
        return payload.get("type")
    # Custom: look for common keys.
    return payload.get("event") or payload.get("type") or payload.get("action")


def process_event(
    session: Session,
    endpoint: WebhookEndpoint,
    *,
    payload: dict[str, Any],
    signature_valid: bool,
    event_type: str | None = None,
    headers: dict[str, str] | None = None,
) -> WebhookEvent:
    """Record and dispatch an inbound webhook event.

    If the signature is invalid or the event type is filtered, the event is
    stored as ``rejected`` and no processing occurs.
    """
    if event_type is None:
        event_type = _extract_event_type(
            endpoint.source_type, headers or {}, payload
        )

    # Gate: signature must be valid.
    if not signature_valid:
        event = WebhookEvent(
            endpoint_id=endpoint.id,
            event_type=event_type,
            payload=payload,
            signature_valid=False,
            status=EVENT_REJECTED,
            error="Invalid HMAC signature",
        )
        session.add(event)
        session.commit()
        session.refresh(event)
        log.warning("webhook.rejected_bad_signature", endpoint_id=endpoint.id)
        return event

    # Gate: event type filter.
    if endpoint.event_filter and event_type not in endpoint.event_filter:
        event = WebhookEvent(
            endpoint_id=endpoint.id,
            event_type=event_type,
            payload=payload,
            signature_valid=True,
            status=EVENT_REJECTED,
            error=f"Event type {event_type!r} not in filter",
        )
        session.add(event)
        session.commit()
        session.refresh(event)
        return event

    # Accept: record and dispatch.
    event = WebhookEvent(
        endpoint_id=endpoint.id,
        event_type=event_type,
        payload=payload,
        signature_valid=True,
        status=EVENT_RECEIVED,
    )
    session.add(event)
    session.commit()
    session.refresh(event)
    log.info("webhook.event_received", event_id=event.id, type=event_type)

    # Dispatch: trigger a linked task or spawn an ad-hoc run.
    _dispatch(session, endpoint, event)
    return event


def _dispatch(session: Session, endpoint: WebhookEndpoint, event: WebhookEvent) -> None:
    """Trigger the configured action for an accepted event."""
    event.status = EVENT_PROCESSING
    session.add(event)
    session.commit()

    try:
        if endpoint.task_id is not None:
            _dispatch_task(session, endpoint, event)
        elif endpoint.prompt_template:
            _dispatch_adhoc(session, endpoint, event)
        else:
            # No action configured — just record it.
            event.status = EVENT_COMPLETED
            session.add(event)
            session.commit()
    except Exception as exc:
        event.status = EVENT_FAILED
        event.error = str(exc)[:1000]
        session.add(event)
        session.commit()
        log.error("webhook.dispatch_failed", event_id=event.id, error=str(exc))


def _dispatch_task(session: Session, endpoint: WebhookEndpoint, event: WebhookEvent) -> None:
    """Trigger the linked scheduled task."""
    from app.models.task import TRIGGER_SOURCE_MANUAL, ScheduledTask
    from app.tasks.service import schedule_task_execution

    task = session.get(ScheduledTask, endpoint.task_id)
    if task is None:
        event.status = EVENT_FAILED
        event.error = f"Linked task {endpoint.task_id} not found"
        session.add(event)
        session.commit()
        return

    run = schedule_task_execution(session, task, trigger_source=TRIGGER_SOURCE_MANUAL)
    event.task_run_id = run.id
    event.status = EVENT_COMPLETED
    session.add(event)
    session.commit()
    log.info("webhook.task_triggered", event_id=event.id, task_id=task.id, run_id=run.id)


def _dispatch_adhoc(session: Session, endpoint: WebhookEndpoint, event: WebhookEvent) -> None:
    """Spawn an ad-hoc agent run with the event context injected into the prompt."""
    from app.agent.service import get_or_create_default_user
    from app.models.task import TRIGGER_SOURCE_MANUAL
    from app.tasks.service import create_task, schedule_task_execution

    # Build the prompt from the template.
    event_json = json.dumps(event.payload or {}, default=str, ensure_ascii=False)[:4000]
    prompt = (endpoint.prompt_template or "Process this event: {event}").replace(
        "{event}", event_json
    )

    user = get_or_create_default_user(session)
    assert user.id is not None

    # Create a one-shot task to handle this event (disabled so it doesn't recur).
    task = create_task(
        session,
        user_id=user.id,
        name=f"[Webhook] {endpoint.name}: {event.event_type or 'event'}",
        prompt=prompt,
        trigger_type="date",
        run_at=datetime.now(UTC),
        enabled=False,  # one-shot, never recurs
    )
    run = schedule_task_execution(session, task, trigger_source=TRIGGER_SOURCE_MANUAL)
    event.task_run_id = run.id
    event.status = EVENT_COMPLETED
    session.add(event)
    session.commit()
    log.info("webhook.adhoc_triggered", event_id=event.id, task_id=task.id)


# --- Event queries --------------------------------------------------------


def list_events(
    session: Session,
    *,
    endpoint_id: int | None = None,
    status: str | None = None,
    limit: int = 50,
) -> Sequence[WebhookEvent]:
    """Events, newest first."""
    stmt = select(WebhookEvent).order_by(col(WebhookEvent.id).desc())
    if endpoint_id is not None:
        stmt = stmt.where(WebhookEvent.endpoint_id == endpoint_id)
    if status is not None:
        stmt = stmt.where(WebhookEvent.status == status)
    return session.exec(stmt.limit(limit)).all()


def get_event(session: Session, event_id: int) -> WebhookEvent | None:
    return session.get(WebhookEvent, event_id)


def replay_event(session: Session, event_id: int) -> WebhookEvent | None:
    """Re-process a stored event (as if it just arrived with a valid signature)."""
    event = session.get(WebhookEvent, event_id)
    if event is None:
        return None
    endpoint = session.get(WebhookEndpoint, event.endpoint_id)
    if endpoint is None:
        return None

    # Create a new event row for the replay.
    new_event = WebhookEvent(
        endpoint_id=endpoint.id,
        event_type=event.event_type,
        payload=event.payload,
        signature_valid=True,
        status=EVENT_RECEIVED,
    )
    session.add(new_event)
    session.commit()
    session.refresh(new_event)
    log.info("webhook.replay", original_id=event_id, new_id=new_event.id)

    _dispatch(session, endpoint, new_event)
    session.refresh(new_event)
    return new_event
