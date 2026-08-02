"""Deep Research workflow models (Фаза 4 — Deep Research).

A ``ResearchRun`` is a durable record of one deep-research workflow
execution: topic decomposition, parallel researcher subagents, collected
sources with confidence/conflict annotations, and the synthesized report
with clickable citations. The report is stored both as markdown text and as
an ``Artifact`` (kind=report) so it lands in the artifact library.

Progress is observable via dedicated SSE events (research_*); subagent runs
spawned by the orchestrator are tracked in ``subagent_runs`` as usual.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin

# ResearchRun status values.
RESEARCH_STATUS_QUEUED = "queued"
RESEARCH_STATUS_RUNNING = "running"
RESEARCH_STATUS_COMPLETED = "completed"
RESEARCH_STATUS_FAILED = "failed"
RESEARCH_STATUS_CANCELLED = "cancelled"

RESEARCH_STATUSES = frozenset(
    {
        RESEARCH_STATUS_QUEUED,
        RESEARCH_STATUS_RUNNING,
        RESEARCH_STATUS_COMPLETED,
        RESEARCH_STATUS_FAILED,
        RESEARCH_STATUS_CANCELLED,
    }
)

TERMINAL_RESEARCH_STATUSES = frozenset(
    {RESEARCH_STATUS_COMPLETED, RESEARCH_STATUS_FAILED, RESEARCH_STATUS_CANCELLED}
)

# Default decomposition depth (number of sub-questions).
RESEARCH_DEPTH_DEFAULT = 4
RESEARCH_DEPTH_MIN = 3
RESEARCH_DEPTH_MAX = 5

# How many researcher subagents run concurrently (cap per research run).
RESEARCH_MAX_CONCURRENT_SUBAGENTS = 3


class ResearchRun(TimestampMixin, table=True):
    """One deep research workflow execution."""

    __tablename__ = "research_runs"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    # Optional link to the conversation that initiated the research.
    conversation_id: int | None = Field(default=None, foreign_key="conversations.id", index=True)
    # Optional link to a recurring-task run that triggered this research.
    parent_task_run_id: int | None = Field(default=None, foreign_key="task_runs.id", index=True)
    # The research question / topic.
    topic: str = Field(sa_column=Column(Text))
    # Number of sub-questions the topic is decomposed into.
    depth: int = RESEARCH_DEPTH_DEFAULT
    # Model used for decomposition + synthesis (subagents may reuse it).
    model: str | None = None
    # See RESEARCH_STATUSES above.
    status: str = Field(default=RESEARCH_STATUS_QUEUED, index=True)
    # Decomposed sub-questions (set once the decomposition stage completes).
    sub_questions: list[str] | None = Field(default=None, sa_column=Column("sub_questions", JSON))
    # Collected sources: [{url, title, snippet, fetched_at, confidence, conflict}].
    sources: list[dict[str, Any]] | None = Field(default=None, sa_column=Column("sources", JSON))
    # Report citations: [{index, text, source_ids: [...], confidence, conflict}].
    citations: list[dict[str, Any]] | None = Field(
        default=None, sa_column=Column("citations", JSON)
    )
    # Final synthesized report (markdown, with [n] citation markers).
    report_markdown: str | None = Field(default=None, sa_column=Column(Text))
    # Artifact (kind=report) holding the report in the artifact library.
    report_artifact_id: int | None = Field(default=None, foreign_key="artifacts.id", index=True)
    # Cumulative token/cost usage across decomposition + synthesis.
    usage: dict[str, Any] | None = Field(default=None, sa_column=Column("usage", JSON))
    # Error message (set on failure).
    error: str | None = Field(default=None, sa_column=Column(Text))
    # SHA-256 of (topic, depth, model) — lets rerun/compare group same-input runs.
    input_hash: str | None = Field(default=None, index=True)
    finished_at: datetime | None = None
