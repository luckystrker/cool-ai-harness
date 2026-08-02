"""Webhook endpoint and event models (Фаза 3b §7 — Webhook Router).

A ``WebhookEndpoint`` is a user-created inbound URL that external systems
(GitHub, Notion, Slack, custom) POST events to. Each endpoint has a unique
``hook_id`` (UUID in the URL path) and an HMAC secret for signature
verification.

A ``WebhookEvent`` is one received payload: its type, validation outcome,
processing status, and the task run it spawned (if any).
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin, _utcnow

# --- Source types ---
SOURCE_GITHUB = "github"
SOURCE_NOTION = "notion"
SOURCE_SLACK = "slack"
SOURCE_CUSTOM = "custom"

SOURCE_TYPES = frozenset({SOURCE_GITHUB, SOURCE_NOTION, SOURCE_SLACK, SOURCE_CUSTOM})

# --- Event processing statuses ---
EVENT_RECEIVED = "received"
EVENT_PROCESSING = "processing"
EVENT_COMPLETED = "completed"
EVENT_FAILED = "failed"
EVENT_REJECTED = "rejected"  # signature invalid or event type filtered

EVENT_STATUSES = frozenset(
    {EVENT_RECEIVED, EVENT_PROCESSING, EVENT_COMPLETED, EVENT_FAILED, EVENT_REJECTED}
)


class WebhookEndpoint(TimestampMixin, table=True):
    """A user-created inbound webhook endpoint."""

    __tablename__ = "webhook_endpoints"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    name: str = Field(index=True)
    # Unique identifier used in the public URL: POST /api/webhooks/{hook_id}
    hook_id: str = Field(unique=True, index=True)
    # HMAC-SHA256 secret for signature verification.
    secret: str
    # What kind of source this endpoint expects (affects header parsing).
    source_type: str = Field(default=SOURCE_CUSTOM, index=True)
    # JSON list of event types to accept; null/empty = accept all.
    event_filter: list[str] | None = Field(default=None, sa_column=Column("event_filter", JSON))
    # Optional: auto-trigger this scheduled task on each event.
    task_id: int | None = Field(default=None, foreign_key="scheduled_tasks.id")
    # Optional: ad-hoc prompt template; {event} is replaced with the payload.
    prompt_template: str | None = Field(default=None, sa_column=Column(Text))
    enabled: bool = Field(default=True, index=True)


class WebhookEvent(TimestampMixin, table=True):
    """One inbound webhook event."""

    __tablename__ = "webhook_events"

    id: int | None = Field(default=None, primary_key=True)
    endpoint_id: int = Field(foreign_key="webhook_endpoints.id", index=True)
    # Event type as reported by the source (e.g. "push", "pull_request").
    event_type: str | None = Field(default=None, index=True)
    # Full JSON payload.
    payload: dict[str, Any] | None = Field(default=None, sa_column=Column("payload", JSON))
    # Whether the HMAC signature was valid.
    signature_valid: bool = False
    # Processing lifecycle.
    status: str = Field(default=EVENT_RECEIVED, index=True)
    # The task run spawned to handle this event (if any).
    task_run_id: int | None = Field(default=None, foreign_key="task_runs.id")
    error: str | None = Field(default=None, sa_column=Column(Text))
    received_at: datetime = Field(default_factory=_utcnow, nullable=False)
