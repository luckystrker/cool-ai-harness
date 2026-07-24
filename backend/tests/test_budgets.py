"""Tests for cost budgets: policy, service accounting, executor block (Фаза 1.5 §5)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest
from sqlmodel import Session, SQLModel, create_engine

from app.budgets import service as budgets
from app.providers import Usage
from app.security.cost import BudgetConfig, BudgetStatus, evaluate_budget


# --- policy: evaluate_budget ------------------------------------------------


def test_no_limits_is_ok() -> None:
    ev = evaluate_budget({"daily": 5.0, "weekly": 10.0, "monthly": 20.0}, BudgetConfig())
    assert ev.status == BudgetStatus.OK


def test_alert_at_threshold() -> None:
    cfg = BudgetConfig(daily_limit_usd=10.0, alert_threshold_pct=80.0)
    ev = evaluate_budget({"daily": 8.0, "weekly": 0.0, "monthly": 0.0}, cfg)
    assert ev.status == BudgetStatus.ALERT
    assert ev.windows["daily"].pct == pytest.approx(80.0)


def test_blocked_when_exceeded_and_block_on() -> None:
    cfg = BudgetConfig(daily_limit_usd=10.0, block_on_exceed=True)
    ev = evaluate_budget({"daily": 11.0, "weekly": 0.0, "monthly": 0.0}, cfg)
    assert ev.status == BudgetStatus.BLOCKED
    assert ev.blocked


def test_not_blocked_when_block_off() -> None:
    cfg = BudgetConfig(daily_limit_usd=10.0, block_on_exceed=False)
    ev = evaluate_budget({"daily": 11.0, "weekly": 0.0, "monthly": 0.0}, cfg)
    # Exceeded but not blocking → still alerts.
    assert ev.status == BudgetStatus.ALERT
    assert not ev.blocked


def test_override_lifts_block() -> None:
    future = datetime.now(UTC) + timedelta(hours=1)
    cfg = BudgetConfig(daily_limit_usd=10.0, block_on_exceed=True, override_until=future)
    ev = evaluate_budget({"daily": 11.0, "weekly": 0.0, "monthly": 0.0}, cfg)
    assert ev.overridden is True
    assert not ev.blocked


def test_expired_override_does_not_lift() -> None:
    past = datetime.now(UTC) - timedelta(hours=1)
    cfg = BudgetConfig(daily_limit_usd=10.0, block_on_exceed=True, override_until=past)
    ev = evaluate_budget({"daily": 11.0, "weekly": 0.0, "monthly": 0.0}, cfg)
    assert ev.overridden is False
    assert ev.blocked


def test_most_relevant_window_blocks() -> None:
    """Exceeding any window blocks, even if another window is fine."""
    cfg = BudgetConfig(monthly_limit_usd=100.0, block_on_exceed=True)
    ev = evaluate_budget({"daily": 0.0, "weekly": 0.0, "monthly": 101.0}, cfg)
    assert ev.status == BudgetStatus.BLOCKED


# --- service: accounting ----------------------------------------------------


@pytest.fixture()
def db_session():
    """An isolated in-memory SQLite session with a user row."""
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False})
    SQLModel.metadata.create_all(engine)
    with Session(engine) as session:
        from app.models import User

        session.add(User(username="t"))
        session.commit()
        yield session


def test_get_budget_creates_default(db_session) -> None:
    row = budgets.get_budget(db_session)
    assert row.id is not None
    assert row.daily_limit_usd is None
    assert row.alert_threshold_pct == 80.0
    assert row.block_on_exceed is True


def test_upsert_and_sum_spend(db_session) -> None:
    budgets.upsert_budget(db_session, daily_limit_usd=1.0, monthly_limit_usd=10.0)
    row = budgets.get_budget(db_session)
    assert row.daily_limit_usd == 1.0
    assert row.monthly_limit_usd == 10.0

    usage = Usage(prompt_tokens=100, completion_tokens=50, total_tokens=150, cost_usd=0.5)
    budgets.record_spend(
        db_session,
        user_id=budgets.DEFAULT_USER_ID,
        model="gpt-4o",
        provider_name="openai",
        usage=usage,
    )
    today = budgets.sum_spend(db_session, budgets.window_start("daily"))
    assert today == pytest.approx(0.5)


def test_budget_evaluation_blocked_after_spend(db_session) -> None:
    budgets.upsert_budget(db_session, daily_limit_usd=1.0, block_on_exceed=True)
    # Spend past the limit.
    budgets.record_spend(
        db_session,
        user_id=budgets.DEFAULT_USER_ID,
        model="gpt-4o",
        provider_name="openai",
        usage=Usage(cost_usd=1.5),
    )
    ev = budgets.budget_evaluation(db_session)
    assert ev.status == BudgetStatus.BLOCKED
    assert budgets.is_blocked(db_session) is True


def test_override_clears_block(db_session) -> None:
    budgets.upsert_budget(db_session, daily_limit_usd=1.0, block_on_exceed=True)
    budgets.record_spend(
        db_session,
        user_id=budgets.DEFAULT_USER_ID,
        model="gpt-4o",
        provider_name="openai",
        usage=Usage(cost_usd=2.0),
    )
    assert budgets.is_blocked(db_session)

    future = datetime.now(UTC) + timedelta(hours=2)
    budgets.set_override(db_session, future)
    assert not budgets.is_blocked(db_session)

    # Clearing the override re-blocks.
    budgets.set_override(db_session, None)
    assert budgets.is_blocked(db_session)


def test_alert_threshold_status(db_session) -> None:
    budgets.upsert_budget(db_session, daily_limit_usd=1.0)
    # 80% of $1 = $0.8 → alert (not yet blocked).
    budgets.record_spend(
        db_session,
        user_id=budgets.DEFAULT_USER_ID,
        model="gpt-4o",
        provider_name="openai",
        usage=Usage(cost_usd=0.8),
    )
    ev = budgets.budget_evaluation(db_session)
    assert ev.status == BudgetStatus.ALERT


def test_list_spend_newest_first(db_session) -> None:
    for c in (0.1, 0.2, 0.3):
        budgets.record_spend(
            db_session,
            user_id=budgets.DEFAULT_USER_ID,
            model="gpt-4o",
            provider_name="openai",
            usage=Usage(cost_usd=c),
        )
    rows = budgets.list_spend(db_session, limit=10)
    assert len(rows) == 3
    assert rows[0].cost_usd >= rows[-1].cost_usd


# --- executor integration: budget block -------------------------------------


@pytest.mark.asyncio
async def test_executor_blocks_when_budget_exceeded(db_session, scripted_provider) -> None:
    """When the budget is exceeded, the loop finishes with reason=budget_exceeded
    before any LLM call."""
    from app.agent import AgentConfig, AgentExecutor
    from app.core.db import engine as real_engine

    # Seed the real engine's DB with an exceeded budget. The executor opens its
    # own session against the configured engine, so we must write there.
    from sqlmodel import Session as _S

    with _S(real_engine) as s:
        budgets.upsert_budget(s, daily_limit_usd=1.0, block_on_exceed=True)
        budgets.record_spend(
            s,
            user_id=budgets.DEFAULT_USER_ID,
            model="gpt-4o",
            provider_name="openai",
            usage=Usage(cost_usd=2.0),
        )

    scripted_provider.set_script(["should not be reached"])
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(model="m", user_id=budgets.DEFAULT_USER_ID),
    )
    events = [e async for e in ex.stream("hi")]
    finish = events[-1]
    assert finish.kind == "finish"
    assert finish.payload["reason"] == "budget_exceeded"
    # Provider was never called — we blocked before the LLM round-trip.
    assert scripted_provider.calls == []

    # Cleanup so other tests don't inherit the exceeded budget.
    with _S(real_engine) as s:
        budgets.set_override(s, None)
        budgets.upsert_budget(
            s, daily_limit_usd=None, weekly_limit_usd=None, monthly_limit_usd=None
        )
        from app.models import SpendLog
        rows = s.exec(__import__("sqlmodel").select(SpendLog)).all()
        for r in rows:
            s.delete(r)
        s.commit()
