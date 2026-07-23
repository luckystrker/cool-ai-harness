"""Approval audit model (Фаза 1.5 §2 — approval audit trail).

Every tool-call approval decision (user-approved, user-denied, or auto-denied
via timeout) is recorded as an ApprovalAudit row. This gives a durable,
queryable history of what dangerous actions the agent attempted and how they
were resolved — essential for security review and compliance.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin


class ApprovalAudit(TimestampMixin, table=True):
    """One row per tool-call approval decision (user or auto-timeout).

    Written by the runner when the executor's approval future resolves (or
    times out). Lets the UI show an approval history and lets admins audit
    what dangerous actions the agent attempted.
    """

    __tablename__ = "approval_audits"

    id: int | None = Field(default=None, primary_key=True)
    conversation_id: int = Field(foreign_key="conversations.id", index=True)
    run_id: int | None = Field(default=None, foreign_key="agent_runs.id", index=True)
    # The tool-call ID the executor used to register the approval future.
    call_id: str
    # Which tool was being gated.
    tool_name: str
    # The arguments the model supplied (masked if secret masking is on).
    arguments: dict[str, Any] | None = Field(default=None, sa_column=Column("arguments", JSON))
    # The decision: True = approved, False = denied.
    approved: bool
    # Who/what made the decision: "user", "timeout", "auto" (auto_approve).
    decision_source: str = Field(default="user")
    # User identifier (for multi-user; MVP is always "default").
    decided_by: str | None = None
    # Why the approval was required (the reason string from the event).
    reason: str | None = Field(default=None, sa_column=Column(Text))
    # Whether this was triggered by a breakpoint (vs a regular "ask" tool).
    is_breakpoint: bool = False
    # Breakpoint type, if applicable.
    breakpoint_type: str | None = None
    # How long the approval took to resolve (ms), or None if auto.
    duration_ms: int | None = None
