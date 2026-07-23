"""Agent run and run-event models (Фаза 1.5 — durable agent runs).

An ``AgentRun`` is a single execution of the agent loop against a conversation:
its status, cumulative token/cost usage, iteration count, a checkpoint of the
last completed step, and the terminal outcome (finish reason / error).

``RunEvent`` is the append-only event log: one row per ``AgentEvent`` the loop
emitted, in order (``seq``), keyed by run. Together they make a run observable,
resumable-aware, and replayable (full replay lands with the Inspector, Фаза 1.5 §6).
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin, _utcnow

# Status values for AgentRun.status. Kept as plain strings (not an enum) so the
# value survives a JSON round-trip and stays human-readable in the DB.
RUN_STATUS_QUEUED = "queued"
RUN_STATUS_RUNNING = "running"
RUN_STATUS_AWAITING_APPROVAL = "awaiting_approval"
RUN_STATUS_COMPLETED = "completed"
RUN_STATUS_FAILED = "failed"
RUN_STATUS_CANCELLED = "cancelled"

RUN_STATUSES = frozenset(
    {
        RUN_STATUS_QUEUED,
        RUN_STATUS_RUNNING,
        RUN_STATUS_AWAITING_APPROVAL,
        RUN_STATUS_COMPLETED,
        RUN_STATUS_FAILED,
        RUN_STATUS_CANCELLED,
    }
)

# Terminal statuses — once set, the run is done and no longer cancellable.
TERMINAL_STATUSES = frozenset({RUN_STATUS_COMPLETED, RUN_STATUS_FAILED, RUN_STATUS_CANCELLED})

# Map a executor finish reason to a run status.
_FINISH_REASON_TO_STATUS: dict[str, str] = {
    "stop": RUN_STATUS_COMPLETED,
    "end_turn": RUN_STATUS_COMPLETED,
    "tool_limit": RUN_STATUS_COMPLETED,
    "token_limit": RUN_STATUS_COMPLETED,
    "cost_limit": RUN_STATUS_COMPLETED,
    "max_iterations": RUN_STATUS_COMPLETED,
    "cancelled": RUN_STATUS_CANCELLED,
    "error": RUN_STATUS_FAILED,
}


def finish_reason_to_status(reason: str | None) -> str:
    """Translate an executor finish reason into a run status.

    Unknown / falsy reasons default to ``completed`` (the loop reached a
    natural stop); ``cancelled`` and ``error`` map to their statuses so a
    cancelled or failed run is distinguishable from a successful one.
    """
    if not reason:
        return RUN_STATUS_COMPLETED
    return _FINISH_REASON_TO_STATUS.get(reason, RUN_STATUS_COMPLETED)


class AgentRun(TimestampMixin, table=True):
    """One execution of the agent loop against a conversation."""

    __tablename__ = "agent_runs"

    id: int | None = Field(default=None, primary_key=True)
    conversation_id: int = Field(foreign_key="conversations.id", index=True)
    user_id: int | None = Field(default=None, foreign_key="users.id", index=True)
    # See RUN_STATUSES above.
    status: str = Field(default=RUN_STATUS_QUEUED, index=True)
    # Which model this run used (snapshot at start time).
    model: str | None = None
    # Run configuration snapshot: {max_iterations, max_total_tokens,
    # max_cost_usd, tool_names, ...}. Lets an observer reconstruct what limits
    # were in effect without relying on the live config.
    config: dict[str, Any] | None = Field(default=None, sa_column=Column("config", JSON))
    # Last completed step: {iteration, last_call_id}. Updated after each tool
    # call so a future resume knows where to pick up.
    checkpoint: dict[str, Any] | None = Field(default=None, sa_column=Column("checkpoint", JSON))
    # Cumulative token/cost usage across all LLM round-trips in this run.
    usage: dict[str, Any] | None = Field(default=None, sa_column=Column("usage", JSON))
    # How many LLM iterations the loop completed.
    iterations: int = 0
    # Terminal outcome (stop|token_limit|cost_limit|max_iterations|cancelled|error).
    finish_reason: str | None = None
    error: str | None = Field(default=None, sa_column=Column(Text))
    # Wall-clock start (DB time, independent of monotonic elapsed_ms tracking).
    started_at: datetime = Field(default_factory=_utcnow, nullable=False)
    finished_at: datetime | None = None


class RunEvent(TimestampMixin, table=True):
    """Append-only event log: one row per AgentEvent emitted during a run."""

    __tablename__ = "run_events"

    id: int | None = Field(default=None, primary_key=True)
    run_id: int = Field(foreign_key="agent_runs.id", index=True)
    # Monotonic per-run sequence (0, 1, 2, ...). Events replay in this order.
    seq: int
    # The AgentEvent.kind this row records (start, token, tool_result, ...).
    kind: str
    # The AgentEvent.payload, verbatim. None only if the event had no payload.
    payload: dict[str, Any] | None = Field(default=None, sa_column=Column("payload", JSON))
