"""Analytics dashboard API routes (Фаза 3a §5 — Observability).

Endpoints for aggregated spend, tool usage, LLM latency, unified call
history, and memory activity timeseries. All data is read-only and derived
from existing tables (SpendLog, ToolCall, RunEvent, MemoryItem).
"""

from __future__ import annotations

from fastapi import APIRouter, Query
from pydantic import BaseModel
from sqlmodel import Session

from app.analytics import (
    call_history,
    llm_latency,
    memory_activity,
    spend_by_model,
    spend_over_time,
    summary_stats,
    top_tools,
)
from app.core.db import engine

router = APIRouter(prefix="/analytics", tags=["analytics"])


# --- Response schemas ---


class SummaryStatsOut(BaseModel):
    total_spend_usd: float
    total_llm_calls: int
    total_tokens: int
    total_tool_calls: int
    tool_error_count: int
    tool_success_rate: float
    days: int


class TimeSeriesPoint(BaseModel):
    period: str
    cost_usd: float = 0.0
    total_tokens: int = 0
    calls: int = 0


class ModelSpendOut(BaseModel):
    model: str
    cost_usd: float
    total_tokens: int
    calls: int


class TopToolOut(BaseModel):
    name: str
    calls: int
    avg_duration_ms: float
    success_rate: float
    error_count: int


class LatencyPoint(BaseModel):
    period: str
    avg_ms: float
    min_ms: int
    max_ms: int
    calls: int


class CallHistoryRow(BaseModel):
    id: int
    ts: str | None = None
    model: str
    provider_name: str
    prompt_tokens: int
    completion_tokens: int
    total_tokens: int
    cost_usd: float
    run_id: int | None = None
    conversation_id: int | None = None


class CallHistoryOut(BaseModel):
    rows: list[CallHistoryRow]
    total: int


class MemoryActivityPoint(BaseModel):
    period: str
    created: int
    by_type: dict[str, int] = {}


# --- Endpoints ---


@router.get("/summary", response_model=SummaryStatsOut)
def get_summary(
    days: int = Query(default=30, ge=1, le=365),
) -> SummaryStatsOut:
    """High-level summary stats for the analytics overview card."""
    with Session(engine) as session:
        data = summary_stats(session, days=days)
    return SummaryStatsOut(**data)


@router.get("/spend-over-time", response_model=list[TimeSeriesPoint])
def get_spend_over_time(
    days: int = Query(default=30, ge=1, le=365),
    bucket: str = Query(default="day", pattern="^(day|hour)$"),
) -> list[TimeSeriesPoint]:
    """Spend aggregated into time buckets (day or hour)."""
    with Session(engine) as session:
        data = spend_over_time(session, days=days, bucket=bucket)
    return [TimeSeriesPoint(**d) for d in data]


@router.get("/spend-by-model", response_model=list[ModelSpendOut])
def get_spend_by_model(
    days: int = Query(default=30, ge=1, le=365),
) -> list[ModelSpendOut]:
    """Spend grouped by model."""
    with Session(engine) as session:
        data = spend_by_model(session, days=days)
    return [ModelSpendOut(**d) for d in data]


@router.get("/top-tools", response_model=list[TopToolOut])
def get_top_tools(
    days: int = Query(default=30, ge=1, le=365),
    limit: int = Query(default=20, ge=1, le=100),
) -> list[TopToolOut]:
    """Most-used tools with success rate and avg duration."""
    with Session(engine) as session:
        data = top_tools(session, days=days, limit=limit)
    return [TopToolOut(**d) for d in data]


@router.get("/latency", response_model=list[LatencyPoint])
def get_latency(
    days: int = Query(default=30, ge=1, le=365),
    bucket: str = Query(default="day", pattern="^(day|hour)$"),
) -> list[LatencyPoint]:
    """LLM call latency aggregated by time bucket."""
    with Session(engine) as session:
        data = llm_latency(session, days=days, bucket=bucket)
    return [LatencyPoint(**d) for d in data]


@router.get("/call-history", response_model=CallHistoryOut)
def get_call_history(
    limit: int = Query(default=100, ge=1, le=1000),
    offset: int = Query(default=0, ge=0),
    model: str | None = Query(default=None),
    provider: str | None = Query(default=None),
) -> CallHistoryOut:
    """Unified LLM call log with pagination and optional filters."""
    with Session(engine) as session:
        rows, total = call_history(
            session, limit=limit, offset=offset, model=model, provider=provider
        )
    return CallHistoryOut(
        rows=[CallHistoryRow(**r) for r in rows],
        total=total,
    )


@router.get("/memory-activity", response_model=list[MemoryActivityPoint])
def get_memory_activity(
    days: int = Query(default=30, ge=1, le=365),
    bucket: str = Query(default="day", pattern="^(day|hour)$"),
) -> list[MemoryActivityPoint]:
    """Memory item creation activity over time."""
    with Session(engine) as session:
        data = memory_activity(session, days=days, bucket=bucket)
    return [MemoryActivityPoint(**d) for d in data]
