"""Scheduled task and task run models (Фаза 3b — Recurring tasks / cron jobs).

A ``ScheduledTask`` is a recurring (cron/interval) or one-shot (date) job
definition: what prompt to run, which profile/model/tools execute it, where the
result is delivered, and the safety limits that apply. It is the durable source
of truth for the scheduler — jobs are rebuilt from this table on every startup,
so schedules survive restarts.

A ``TaskRun`` is one execution attempt: it owns an isolated conversation (like a
subagent), links to the durable ``AgentRun``, and records output, usage,
approval decision, delivery status, and read state for the inbox.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin, _utcnow

# --- Trigger kinds ---
TRIGGER_CRON = "cron"
TRIGGER_INTERVAL = "interval"
TRIGGER_DATE = "date"  # one-shot reminder

TRIGGER_TYPES = frozenset({TRIGGER_CRON, TRIGGER_INTERVAL, TRIGGER_DATE})

# --- TaskRun status values (plain strings, same convention as AgentRun) ---
TASK_RUN_QUEUED = "queued"
TASK_RUN_RUNNING = "running"
TASK_RUN_COMPLETED = "completed"
TASK_RUN_FAILED = "failed"
TASK_RUN_CANCELLED = "cancelled"
# Fired but deliberately not executed (quiet hours / misfire policy / disabled).
TASK_RUN_SKIPPED = "skipped"

TASK_RUN_STATUSES = frozenset(
    {
        TASK_RUN_QUEUED,
        TASK_RUN_RUNNING,
        TASK_RUN_COMPLETED,
        TASK_RUN_FAILED,
        TASK_RUN_CANCELLED,
        TASK_RUN_SKIPPED,
    }
)

TERMINAL_TASK_RUN_STATUSES = frozenset(
    {TASK_RUN_COMPLETED, TASK_RUN_FAILED, TASK_RUN_CANCELLED, TASK_RUN_SKIPPED}
)

# --- Where a run came from ---
TRIGGER_SOURCE_SCHEDULE = "schedule"
TRIGGER_SOURCE_MANUAL = "manual"
TRIGGER_SOURCE_AGENT = "agent"

# --- Misfire policy: what to do when a fire time was missed (downtime) ---
MISFIRE_SKIP = "skip"
MISFIRE_RUN = "run"

# --- Approval policy for tools with an external side effect ---
# "deny_external" — send_external is denied for background runs (default: a
#   scheduled job never messages the outside world without explicit opt-in).
# "allow_all" — the user pre-approved external side effects for this task.
APPROVAL_DENY_EXTERNAL = "deny_external"
APPROVAL_ALLOW_ALL = "allow_all"

APPROVAL_POLICIES = frozenset({APPROVAL_DENY_EXTERNAL, APPROVAL_ALLOW_ALL})

# --- Delivery channels ---
CHANNEL_UI = "ui"
CHANNEL_WEBHOOK = "webhook"
CHANNEL_TELEGRAM = "telegram"
CHANNEL_EMAIL = "email"

DELIVERY_CHANNELS = frozenset({CHANNEL_UI, CHANNEL_WEBHOOK, CHANNEL_TELEGRAM, CHANNEL_EMAIL})


class ScheduledTask(TimestampMixin, table=True):
    """A recurring or one-shot agent job definition."""

    __tablename__ = "scheduled_tasks"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    name: str = Field(index=True)
    description: str | None = Field(default=None, sa_column=Column(Text))

    # --- Schedule ---
    # See TRIGGER_TYPES. cron uses cron_expression, interval uses
    # interval_seconds, date uses run_at (one-shot reminder).
    trigger_type: str = Field(default=TRIGGER_CRON, index=True)
    # Standard 5-field cron expression (validated with croniter).
    cron_expression: str | None = None
    interval_seconds: int | None = None
    run_at: datetime | None = None
    # IANA timezone the schedule is interpreted in (e.g. "Europe/Berlin").
    timezone: str = "UTC"
    # Quiet hours in the task's timezone as "HH:MM" strings. A fire time inside
    # the window is skipped (recorded as a skipped TaskRun) instead of running.
    quiet_hours_start: str | None = None
    quiet_hours_end: str | None = None
    # See MISFIRE_SKIP / MISFIRE_RUN.
    misfire_policy: str = MISFIRE_SKIP

    # --- What to run ---
    prompt: str = Field(sa_column=Column(Text))
    # Optional template slug this task was created from (news-digest, ...).
    workflow_type: str | None = None
    # Which personality executes the task (None = default system prompt).
    profile_id: int | None = Field(default=None, foreign_key="agent_profiles.id", index=True)
    model: str | None = None
    # Tool whitelist (None = all registered tools).
    tools_whitelist: list[str] | None = Field(
        default=None, sa_column=Column("tools_whitelist", JSON)
    )
    # Capability policy override for the run, e.g. {"write": "deny"}.
    capability_policy: dict[str, Any] | None = Field(
        default=None, sa_column=Column("capability_policy", JSON)
    )
    working_directory: str | None = None
    # See APPROVAL_POLICIES.
    approval_policy: str = APPROVAL_DENY_EXTERNAL

    # --- Delivery ---
    # Channel list, e.g. ["ui", "webhook"]. Empty/None = UI only.
    delivery_channels: list[str] | None = Field(
        default=None, sa_column=Column("delivery_channels", JSON)
    )
    # Channel configuration, e.g. {"webhook_url": "https://..."}.
    delivery_config: dict[str, Any] | None = Field(
        default=None, sa_column=Column("delivery_config", JSON)
    )
    # Hash of the last delivered output — identical results are not re-delivered
    # (notification deduplication).
    last_delivery_hash: str | None = None

    # --- Limits ---
    max_iterations: int = 10
    max_cost_per_run: float | None = None
    timeout_s: float | None = None

    # --- State ---
    enabled: bool = Field(default=True, index=True)
    next_run_at: datetime | None = Field(default=None, index=True)
    last_run_at: datetime | None = None
    # Status of the most recent run (see TASK_RUN_STATUSES).
    last_status: str | None = None
    run_count: int = 0
    # Consecutive failures; the scheduler auto-disables a task that keeps
    # failing (see settings.scheduler_max_consecutive_failures).
    failure_count: int = 0


class TaskRun(TimestampMixin, table=True):
    """One execution attempt of a ScheduledTask."""

    __tablename__ = "task_runs"

    id: int | None = Field(default=None, primary_key=True)
    task_id: int = Field(foreign_key="scheduled_tasks.id", index=True)
    # Isolated conversation owned by this run (None for skipped runs, which
    # never reach the agent loop).
    conversation_id: int | None = Field(default=None, foreign_key="conversations.id")
    # Durable run row tracking the agent loop execution.
    run_id: int | None = Field(default=None, foreign_key="agent_runs.id")
    # See TASK_RUN_STATUSES.
    status: str = Field(default=TASK_RUN_QUEUED, index=True)
    # schedule | manual | agent.
    trigger_source: str = TRIGGER_SOURCE_SCHEDULE
    # Prompt snapshot (the task's prompt may change later).
    prompt: str = Field(sa_column=Column(Text))
    output: str | None = Field(default=None, sa_column=Column(Text))
    error: str | None = Field(default=None, sa_column=Column(Text))
    # Why a run was skipped (quiet hours, misfire, disabled).
    skip_reason: str | None = None
    # Approval decision applied to external side effects for this run, plus the
    # human-readable reason — persisted so a background approval is auditable.
    approval_policy: str | None = None
    approval_reason: str | None = Field(default=None, sa_column=Column(Text))
    # Cumulative token/cost usage from the agent loop.
    usage: dict[str, Any] | None = Field(default=None, sa_column=Column("usage", JSON))
    duration_ms: int | None = None
    # Per-channel delivery outcome, e.g. {"ui": "ok", "webhook": "failed: ..."}.
    delivery_status: dict[str, Any] | None = Field(
        default=None, sa_column=Column("delivery_status", JSON)
    )
    delivered_at: datetime | None = None
    # Inbox read state (unread runs drive the notification badge).
    is_read: bool = Field(default=False, index=True)
    started_at: datetime = Field(default_factory=_utcnow, nullable=False)
    finished_at: datetime | None = None
