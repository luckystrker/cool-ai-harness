"""Analytics aggregation service (Фаза 3a §5 — Observability dashboards).

Provides pre-aggregated time-series and summary data for the analytics
dashboard: spend-over-time, spend-by-model, top-tools, LLM latency,
unified call history, and memory activity.

All functions are pure queries (read-only) and accept a SQLModel Session.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import Any

from sqlalchemy import func
from sqlalchemy import select as sa_select
from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.models import RunEvent, SpendLog, ToolCall

log = get_logger(__name__)

# Default lookback window for dashboard queries.
DEFAULT_DAYS = 30


# --- Spend aggregations ---


def spend_over_time(
    session: Session,
    *,
    days: int = DEFAULT_DAYS,
    bucket: str = "day",
) -> list[dict]:
    """Spend aggregated into time buckets (day or hour).

    Returns [{period: "2026-07-01", cost_usd: 1.23, total_tokens: 456}, ...].
    """
    since = datetime.now(UTC) - timedelta(days=days)

    if bucket == "hour":
        # SQLite: strftime('%Y-%m-%d %H:00', ts)
        period_expr = func.strftime("%Y-%m-%d %H:00", SpendLog.ts)
    else:
        # SQLite: strftime('%Y-%m-%d', ts)
        period_expr = func.strftime("%Y-%m-%d", SpendLog.ts)

    rows = session.execute(
        sa_select(
            period_expr.label("period"),
            func.sum(SpendLog.cost_usd).label("cost_usd"),
            func.sum(SpendLog.total_tokens).label("total_tokens"),
            func.count().label("calls"),
        )
        .where(col(SpendLog.ts) >= since)
        .group_by(period_expr)
        .order_by(period_expr)
    ).all()

    return [
        {
            "period": r[0],
            "cost_usd": round(float(r[1] or 0), 6),
            "total_tokens": int(r[2] or 0),
            "calls": int(r[3] or 0),
        }
        for r in rows
    ]


def spend_by_model(
    session: Session,
    *,
    days: int = DEFAULT_DAYS,
) -> list[dict]:
    """Spend grouped by model.

    Returns [{model: "gpt-4o", cost_usd: 2.5, total_tokens: 1000, calls: 10}, ...].
    """
    since = datetime.now(UTC) - timedelta(days=days)

    rows = session.execute(
        sa_select(
            col(SpendLog.model),
            func.sum(SpendLog.cost_usd).label("cost_usd"),
            func.sum(SpendLog.total_tokens).label("total_tokens"),
            func.count().label("calls"),
        )
        .where(col(SpendLog.ts) >= since)
        .group_by(col(SpendLog.model))
        .order_by(func.sum(SpendLog.cost_usd).desc())
    ).all()

    return [
        {
            "model": r[0] or "unknown",
            "cost_usd": round(float(r[1] or 0), 6),
            "total_tokens": int(r[2] or 0),
            "calls": int(r[3] or 0),
        }
        for r in rows
    ]


# --- Tool call aggregations ---


def top_tools(
    session: Session,
    *,
    days: int = DEFAULT_DAYS,
    limit: int = 20,
) -> list[dict]:
    """Most-used tools with success rate and avg duration.

    Returns [{name, calls, success_rate, avg_duration_ms, error_count}, ...].
    """
    since = datetime.now(UTC) - timedelta(days=days)

    rows = session.execute(
        sa_select(
            col(ToolCall.name),
            func.count().label("calls"),
            func.avg(ToolCall.duration_ms).label("avg_duration_ms"),
        )
        .where(col(ToolCall.created_at) >= since)
        .group_by(col(ToolCall.name))
        .order_by(func.count().desc())
        .limit(limit)
    ).all()

    results: list[dict[str, Any]] = []
    for r in rows:
        calls = int(r[1] or 0)
        results.append({
            "name": r[0],
            "calls": calls,
            "avg_duration_ms": round(float(r[2] or 0), 1),
        })

    # Compute success rate separately (SQLite boolean handling)
    for item in results:
        success_rows = session.execute(
            sa_select(func.count()).where(
                col(ToolCall.created_at) >= since,
                col(ToolCall.name) == item["name"],
                col(ToolCall.success).is_(True),
            )
        ).scalar_one()
        total = item["calls"]
        success = int(success_rows or 0)
        item["success_rate"] = round(success / total, 3) if total > 0 else 0.0
        item["error_count"] = total - success

    return results


# --- LLM latency ---


def llm_latency(
    session: Session,
    *,
    days: int = DEFAULT_DAYS,
    bucket: str = "day",
) -> list[dict]:
    """LLM call latency from llm_call_complete events, aggregated by time bucket.

    Returns [{period, avg_ms, min_ms, max_ms, calls}, ...].
    """
    since = datetime.now(UTC) - timedelta(days=days)

    if bucket == "hour":
        period_expr = func.strftime("%Y-%m-%d %H:00", RunEvent.created_at)
    else:
        period_expr = func.strftime("%Y-%m-%d", RunEvent.created_at)

    # RunEvent.payload is JSON; duration_ms is at payload->>'duration_ms'.
    # SQLite json_extract works on the JSON column.
    duration_expr = func.json_extract(RunEvent.payload, "$.duration_ms")

    rows = session.execute(
        sa_select(
            period_expr.label("period"),
            func.avg(duration_expr).label("avg_ms"),
            func.min(duration_expr).label("min_ms"),
            func.max(duration_expr).label("max_ms"),
            func.count().label("calls"),
        )
        .where(
            col(RunEvent.kind) == "llm_call_complete",
            col(RunEvent.created_at) >= since,
        )
        .group_by(period_expr)
        .order_by(period_expr)
    ).all()

    return [
        {
            "period": r[0],
            "avg_ms": round(float(r[1] or 0), 1),
            "min_ms": int(r[2] or 0),
            "max_ms": int(r[3] or 0),
            "calls": int(r[4] or 0),
        }
        for r in rows
    ]


# --- Unified call history (LLM calls) ---


def call_history(
    session: Session,
    *,
    limit: int = 100,
    offset: int = 0,
    model: str | None = None,
    provider: str | None = None,
) -> tuple[list[dict], int]:
    """Unified LLM call log from SpendLog (has cost/tokens/model/provider/ts).

    Returns (rows, total_count) for pagination.
    """
    base = select(SpendLog).order_by(col(SpendLog.ts).desc(), col(SpendLog.id).desc())
    count_base = sa_select(func.count()).select_from(SpendLog)

    if model:
        base = base.where(SpendLog.model == model)
        count_base = count_base.where(col(SpendLog.model) == model)
    if provider:
        base = base.where(SpendLog.provider_name == provider)
        count_base = count_base.where(col(SpendLog.provider_name) == provider)

    total = int(session.execute(count_base).scalar_one() or 0)
    rows = session.exec(base.offset(offset).limit(limit)).all()

    return [
        {
            "id": r.id,
            "ts": r.ts.isoformat() if r.ts else None,
            "model": r.model,
            "provider_name": r.provider_name,
            "prompt_tokens": r.prompt_tokens,
            "completion_tokens": r.completion_tokens,
            "total_tokens": r.total_tokens,
            "cost_usd": r.cost_usd,
            "run_id": r.run_id,
            "conversation_id": r.conversation_id,
        }
        for r in rows
    ], total


# --- Memory activity timeseries ---


def memory_activity(
    session: Session,
    *,
    days: int = DEFAULT_DAYS,
    bucket: str = "day",
) -> list[dict]:
    """Memory item creation activity over time.

    Returns [{period, created, by_type: {semantic: N, ...}}, ...].
    """
    from app.memory.models import MemoryItem

    since = datetime.now(UTC) - timedelta(days=days)

    if bucket == "hour":
        period_expr = func.strftime("%Y-%m-%d %H:00", MemoryItem.created_at)
    else:
        period_expr = func.strftime("%Y-%m-%d", MemoryItem.created_at)

    rows = session.execute(
        sa_select(
            period_expr.label("period"),
            col(MemoryItem.memory_type),
            func.count().label("cnt"),
        )
        .where(col(MemoryItem.created_at) >= since)
        .group_by(period_expr, col(MemoryItem.memory_type))
        .order_by(period_expr)
    ).all()

    # Aggregate into per-period dicts with by_type breakdown.
    periods: dict[str, dict] = {}
    for r in rows:
        period = r[0]
        mtype = r[1] or "unknown"
        cnt = int(r[2] or 0)
        if period not in periods:
            periods[period] = {"period": period, "created": 0, "by_type": {}}
        periods[period]["created"] += cnt
        periods[period]["by_type"][mtype] = cnt

    return sorted(periods.values(), key=lambda x: x["period"])


# --- Summary stats (overview card) ---


def summary_stats(
    session: Session,
    *,
    days: int = DEFAULT_DAYS,
) -> dict:
    """High-level summary for the analytics overview card."""
    since = datetime.now(UTC) - timedelta(days=days)

    total_spend = float(
        session.execute(
            sa_select(func.coalesce(func.sum(SpendLog.cost_usd), 0.0)).where(
                col(SpendLog.ts) >= since
            )
        ).scalar_one()
        or 0
    )
    total_calls = int(
        session.execute(sa_select(func.count()).where(col(SpendLog.ts) >= since)).scalar_one()
        or 0
    )
    total_tokens = int(
        session.execute(
            sa_select(func.coalesce(func.sum(SpendLog.total_tokens), 0)).where(
                col(SpendLog.ts) >= since
            )
        ).scalar_one()
        or 0
    )
    total_tool_calls = int(
        session.execute(sa_select(func.count()).where(col(ToolCall.created_at) >= since))
        .scalar_one()
        or 0
    )
    tool_errors = int(
        session.execute(
            sa_select(func.count()).where(
                col(ToolCall.created_at) >= since,
                col(ToolCall.success).is_(False),
            )
        ).scalar_one()
        or 0
    )

    return {
        "total_spend_usd": round(total_spend, 6),
        "total_llm_calls": total_calls,
        "total_tokens": total_tokens,
        "total_tool_calls": total_tool_calls,
        "tool_error_count": tool_errors,
        "tool_success_rate": round(
            (total_tool_calls - tool_errors) / total_tool_calls, 3
        )
        if total_tool_calls > 0
        else 1.0,
        "days": days,
    }
