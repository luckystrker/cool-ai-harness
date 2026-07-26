"""Subagent role and run models (Фаза 2 §5 — Subagents).

A ``SubagentRole`` is a reusable persona/configuration template that defines
how a subagent behaves: its system prompt, preferred model, tool whitelist,
capability restrictions, and safety limits.

A ``SubagentRun`` is a single execution instance: it links a role to a
parent conversation/run, owns an isolated conversation for its history,
and tracks status/result/usage independently of the parent.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin, _utcnow

# SubagentRun status values.
SUBAGENT_STATUS_QUEUED = "queued"
SUBAGENT_STATUS_RUNNING = "running"
SUBAGENT_STATUS_COMPLETED = "completed"
SUBAGENT_STATUS_FAILED = "failed"
SUBAGENT_STATUS_CANCELLED = "cancelled"

SUBAGENT_STATUSES = frozenset(
    {
        SUBAGENT_STATUS_QUEUED,
        SUBAGENT_STATUS_RUNNING,
        SUBAGENT_STATUS_COMPLETED,
        SUBAGENT_STATUS_FAILED,
        SUBAGENT_STATUS_CANCELLED,
    }
)

TERMINAL_SUBAGENT_STATUSES = frozenset(
    {SUBAGENT_STATUS_COMPLETED, SUBAGENT_STATUS_FAILED, SUBAGENT_STATUS_CANCELLED}
)


class SubagentRole(TimestampMixin, table=True):
    """Reusable role definition (persona + config template) for subagents."""

    __tablename__ = "subagent_roles"

    id: int | None = Field(default=None, primary_key=True)
    # Human-readable role name, e.g. "researcher", "code-reviewer".
    name: str = Field(index=True)
    description: str | None = Field(default=None, sa_column=Column(Text))
    # Role-specific system prompt injected into the subagent's context.
    system_prompt: str | None = Field(default=None, sa_column=Column(Text))
    # Preferred model (None = inherit from parent or use default).
    model: str | None = None
    # Tool whitelist (None = all registered tools available).
    tool_names: list[str] | None = Field(default=None, sa_column=Column("tool_names", JSON))
    # Restricted capability policy for this role.
    capability_policy: dict[str, Any] | None = Field(
        default=None, sa_column=Column("capability_policy", JSON)
    )
    # Safety limits.
    max_iterations: int = 10
    max_cost_usd: float | None = None
    # Built-in roles ship with the app and cannot be deleted.
    is_builtin: bool = False


class SubagentRun(TimestampMixin, table=True):
    """One execution instance of a subagent."""

    __tablename__ = "subagent_runs"

    id: int | None = Field(default=None, primary_key=True)
    # Which role definition was used (None = ad-hoc with inline config).
    role_id: int | None = Field(default=None, foreign_key="subagent_roles.id", index=True)
    # The parent conversation that spawned this subagent.
    parent_conversation_id: int = Field(foreign_key="conversations.id", index=True)
    # The parent run that spawned this subagent (if spawned by the agent loop).
    parent_run_id: int | None = Field(default=None, foreign_key="agent_runs.id", index=True)
    # Isolated conversation owned by this subagent (separate history).
    conversation_id: int = Field(foreign_key="conversations.id")
    # Durable run row tracking the subagent's agent loop execution.
    run_id: int | None = Field(default=None, foreign_key="agent_runs.id")
    # Display name for this instance (auto-generated if not provided).
    name: str | None = None
    # The task/prompt given to the subagent.
    prompt: str = Field(sa_column=Column(Text))
    # See SUBAGENT_STATUSES above.
    status: str = Field(default=SUBAGENT_STATUS_QUEUED, index=True)
    # Final output text (set on completion).
    result_summary: str | None = Field(default=None, sa_column=Column(Text))
    # Cumulative token/cost usage.
    usage: dict[str, Any] | None = Field(default=None, sa_column=Column("usage", JSON))
    # Error message (set on failure).
    error: str | None = Field(default=None, sa_column=Column(Text))
    # Wall-clock timing.
    started_at: datetime = Field(default_factory=_utcnow, nullable=False)
    finished_at: datetime | None = None
