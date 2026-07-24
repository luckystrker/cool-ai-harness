"""Cost budget and spend-log models (Фаза 1.5 §5).

``Budget`` — one row per user storing the daily/weekly/monthly USD ceilings,
alert threshold, block behavior, and an optional override that lifts a block
until a given time.

``SpendLog`` — append-only, one row per LLM call. Powers real-time spend
history and the daily/weekly/monthly window roll-ups the budgets UI renders.
"""

from __future__ import annotations

from datetime import UTC, datetime

from sqlmodel import Field

from app.models.base import TimestampMixin


class Budget(TimestampMixin, table=True):
    __tablename__ = "budgets"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    # USD ceilings per window. None = no budget for that window.
    daily_limit_usd: float | None = None
    weekly_limit_usd: float | None = None
    monthly_limit_usd: float | None = None
    # Spend percentage (0–100) at which an alert fires. Copied from settings on
    # create so each user can later tune it independently.
    alert_threshold_pct: float = Field(default=80.0)
    # When True, new LLM calls are blocked once a budget is exceeded (until the
    # override window or the period rolls over).
    block_on_exceed: bool = Field(default=True)
    # When set and in the future, a block is lifted until this timestamp.
    override_until: datetime | None = Field(default=None)
    # Last time an alert fired for the current period (debounces repeat alerts).
    last_alert_at: datetime | None = Field(default=None)


class SpendLog(TimestampMixin, table=True):
    __tablename__ = "spend_log"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    # The run / conversation this charge belongs to (nullable for spend
    # recorded outside an agent run).
    run_id: int | None = Field(default=None, foreign_key="agent_runs.id", index=True)
    conversation_id: int | None = Field(default=None, foreign_key="conversations.id", index=True)
    provider_name: str = Field(default="")
    model: str = Field(default="")
    prompt_tokens: int = Field(default=0)
    completion_tokens: int = Field(default=0)
    total_tokens: int = Field(default=0)
    cost_usd: float = Field(default=0.0)
    # Redundant with created_at but explicit + indexed for windowed queries.
    ts: datetime = Field(default_factory=lambda: datetime.now(UTC), nullable=False, index=True)
