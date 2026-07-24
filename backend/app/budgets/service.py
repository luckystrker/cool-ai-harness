"""Cost budget persistence + spend accounting (Фаза 1.5 §5).

This is the data layer for cost guards: it knows how to read/write the
per-user ``Budget`` row, record per-LLM-call spend into ``spend_log``, sum
spend over a time window, and turn the whole picture into a
``BudgetEvaluation`` (the policy object the executor consumes).

Mirrors the session-scoped-function style of ``app/agent/service.py``. The
executor does not hold a session, so ``record_spend_run_scoped`` opens its own
short-lived session (same pattern as ``app/providers/registry.py``).
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from sqlalchemy import func
from sqlmodel import Session, select

from app.core.config import get_settings
from app.core.db import engine
from app.core.logging import get_logger
from app.models import Budget, SpendLog
from app.providers import Usage
from app.security.cost import (
    BudgetConfig,
    BudgetEvaluation,
    Window,
    evaluate_budget,
)

log = get_logger(__name__)

# MVP single-user (consistent with providers, approvals, etc.).
DEFAULT_USER_ID = 1


# --- window math -----------------------------------------------------------


def _window_start(window: Window, *, now: datetime | None = None) -> datetime:
    """The UTC timestamp at which ``window``'s current period began."""
    now = now or datetime.now(UTC)
    if window == "daily":
        start = now.replace(hour=0, minute=0, second=0, microsecond=0)
    elif window == "weekly":
        # Weeks start Monday 00:00 UTC.
        start = (now - timedelta(days=now.weekday())).replace(
            hour=0, minute=0, second=0, microsecond=0
        )
    elif window == "monthly":
        start = now.replace(day=1, hour=0, minute=0, second=0, microsecond=0)
    else:  # pragma: no cover — Window is a Literal
        raise ValueError(f"unknown window: {window!r}")
    return start


# Public alias (used by the executor for alert debouncing).
window_start = _window_start


# --- budget CRUD -----------------------------------------------------------


def get_budget(session: Session, user_id: int = DEFAULT_USER_ID) -> Budget:
    """Return the user's budget row, creating a default one on first access."""
    row = session.exec(select(Budget).where(Budget.user_id == user_id)).first()
    if row is not None:
        return row
    s = get_settings()
    row = Budget(
        user_id=user_id,
        alert_threshold_pct=s.budget_alert_threshold_pct,
        block_on_exceed=s.budget_block_on_exceed,
    )
    session.add(row)
    session.commit()
    session.refresh(row)
    return row


def upsert_budget(
    session: Session,
    *,
    user_id: int = DEFAULT_USER_ID,
    daily_limit_usd: float | None = None,
    weekly_limit_usd: float | None = None,
    monthly_limit_usd: float | None = None,
    alert_threshold_pct: float | None = None,
    block_on_exceed: bool | None = None,
) -> Budget:
    """Create or update the user's budget row. Only provided fields change."""
    row = get_budget(session, user_id)
    if daily_limit_usd is not None:
        row.daily_limit_usd = daily_limit_usd
    if weekly_limit_usd is not None:
        row.weekly_limit_usd = weekly_limit_usd
    if monthly_limit_usd is not None:
        row.monthly_limit_usd = monthly_limit_usd
    if alert_threshold_pct is not None:
        row.alert_threshold_pct = alert_threshold_pct
    if block_on_exceed is not None:
        row.block_on_exceed = block_on_exceed
    session.add(row)
    session.commit()
    session.refresh(row)
    return row


def set_override(
    session: Session,
    until: datetime | None,
    *,
    user_id: int = DEFAULT_USER_ID,
) -> Budget:
    """Set (``until`` given) or clear (``until=None``) the block override."""
    row = get_budget(session, user_id)
    row.override_until = until
    session.add(row)
    session.commit()
    session.refresh(row)
    return row


def mark_alert_fired(
    session: Session,
    *,
    user_id: int = DEFAULT_USER_ID,
    when: datetime | None = None,
) -> None:
    """Record that an alert fired, debouncing repeat alerts within a period."""
    row = get_budget(session, user_id)
    row.last_alert_at = when or datetime.now(UTC)
    session.add(row)
    session.commit()


# --- spend accounting -------------------------------------------------------


def sum_spend(
    session: Session,
    since: datetime,
    *,
    user_id: int = DEFAULT_USER_ID,
) -> float:
    """Sum USD spend since ``since`` for ``user_id`` (0.0 when no rows)."""
    total = session.exec(
        select(func.coalesce(func.sum(SpendLog.cost_usd), 0.0)).where(
            SpendLog.user_id == user_id,
            SpendLog.ts >= since,
        )
    ).one()
    return float(total)


def spend_by_window(
    session: Session,
    *,
    user_id: int = DEFAULT_USER_ID,
    now: datetime | None = None,
) -> dict[Window, float]:
    """Current daily/weekly/monthly spend for ``user_id``."""
    now = now or datetime.now(UTC)
    return {
        "daily": sum_spend(session, _window_start("daily", now=now), user_id=user_id),
        "weekly": sum_spend(session, _window_start("weekly", now=now), user_id=user_id),
        "monthly": sum_spend(session, _window_start("monthly", now=now), user_id=user_id),
    }


def record_spend(
    session: Session,
    *,
    user_id: int,
    model: str,
    provider_name: str,
    usage: Usage,
    run_id: int | None = None,
    conversation_id: int | None = None,
    cost_usd: float | None = None,
) -> SpendLog:
    """Append one spend row for an LLM call."""
    row = SpendLog(
        user_id=user_id,
        run_id=run_id,
        conversation_id=conversation_id,
        provider_name=provider_name,
        model=model,
        prompt_tokens=usage.prompt_tokens,
        completion_tokens=usage.completion_tokens,
        total_tokens=usage.total_tokens,
        cost_usd=cost_usd if cost_usd is not None else (usage.cost_usd or 0.0),
        ts=datetime.now(UTC),
    )
    session.add(row)
    session.commit()
    session.refresh(row)
    return row


def record_spend_run_scoped(
    *,
    user_id: int,
    model: str,
    provider_name: str,
    usage: Usage,
    run_id: int | None = None,
    conversation_id: int | None = None,
) -> None:
    """Record spend from the agent loop, which doesn't own a session.

    Opens a short-lived session (pattern from app/providers/registry.py). Logs
    and swallows DB errors so a spend-accounting hiccup never breaks a turn.
    """
    try:
        with Session(engine) as session:
            record_spend(
                session,
                user_id=user_id,
                model=model,
                provider_name=provider_name,
                usage=usage,
                run_id=run_id,
                conversation_id=conversation_id,
            )
    except Exception as exc:  # noqa: BLE001 — never break a turn over spend logging
        log.warning("budgets.record_spend_failed", error=str(exc))


# --- evaluation -------------------------------------------------------------


def budget_evaluation(
    session: Session,
    *,
    user_id: int = DEFAULT_USER_ID,
    now: datetime | None = None,
) -> BudgetEvaluation:
    """Full budget picture: config + per-window spend → status."""
    row = get_budget(session, user_id)
    config = BudgetConfig.from_row(row)
    spend = spend_by_window(session, user_id=user_id, now=now)
    return evaluate_budget(spend, config, now=now)


def is_blocked(
    session: Session,
    *,
    user_id: int = DEFAULT_USER_ID,
) -> bool:
    """Convenience: whether new LLM calls should be blocked right now."""
    return budget_evaluation(session, user_id=user_id).blocked


# --- history (for the UI) ---------------------------------------------------


def list_spend(
    session: Session,
    *,
    since: datetime | None = None,
    limit: int = 200,
    user_id: int = DEFAULT_USER_ID,
) -> list[SpendLog]:
    """Recent spend rows, newest first (for the budgets page history view)."""
    stmt = (
        select(SpendLog)
        .where(SpendLog.user_id == user_id)
        .order_by(SpendLog.ts.desc(), SpendLog.id.desc())
        .limit(limit)
    )
    if since is not None:
        stmt = stmt.where(SpendLog.ts >= since)
    return list(session.exec(stmt).all())
