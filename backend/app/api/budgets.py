"""Budgets API: cost-budget configuration, status, override, and spend history.

Endpoints (all under /api, single-user MVP):
  GET    /budgets            — current budget config + live status + per-window spend
  PUT    /budgets            — upsert daily/weekly/monthly limits, threshold, block
  POST   /budgets/override   — lift a block until a given time {until: ISO}
  DELETE /budgets/override   — clear an active override
  GET    /budgets/spend      — recent spend rows (history) ?limit=&since=
"""

from __future__ import annotations

from datetime import UTC, datetime

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field
from sqlmodel import Session

from app.budgets import service as budgets
from app.core.db import get_session
from app.core.logging import get_logger
from app.security.cost import BudgetStatus

log = get_logger(__name__)

router = APIRouter()


# --- schemas ---


class BudgetUpdate(BaseModel):
    daily_limit_usd: float | None = Field(default=None, ge=0)
    weekly_limit_usd: float | None = Field(default=None, ge=0)
    monthly_limit_usd: float | None = Field(default=None, ge=0)
    alert_threshold_pct: float | None = Field(default=None, ge=0, le=100)
    block_on_exceed: bool | None = None


class OverrideRequest(BaseModel):
    until: datetime = Field(..., description="ISO-8601 timestamp until which the block is lifted")


class WindowSpendOut(BaseModel):
    spend_usd: float
    limit_usd: float | None
    pct: float


class BudgetStatusOut(BaseModel):
    status: str  # ok | alert | blocked
    overridden: bool
    daily: WindowSpendOut
    weekly: WindowSpendOut
    monthly: WindowSpendOut
    # Config snapshot
    daily_limit_usd: float | None
    weekly_limit_usd: float | None
    monthly_limit_usd: float | None
    alert_threshold_pct: float
    block_on_exceed: bool
    override_until: datetime | None


class SpendRowOut(BaseModel):
    id: int
    run_id: int | None
    conversation_id: int | None
    provider_name: str
    model: str
    prompt_tokens: int
    completion_tokens: int
    total_tokens: int
    cost_usd: float
    ts: datetime


def _status_out(session: Session) -> BudgetStatusOut:
    evaluation = budgets.budget_evaluation(session)
    return BudgetStatusOut(
        status=evaluation.status.value,
        overridden=evaluation.overridden,
        daily=WindowSpendOut(
            spend_usd=evaluation.windows["daily"].spend_usd,
            limit_usd=evaluation.windows["daily"].limit_usd,
            pct=round(evaluation.windows["daily"].pct, 2),
        ),
        weekly=WindowSpendOut(
            spend_usd=evaluation.windows["weekly"].spend_usd,
            limit_usd=evaluation.windows["weekly"].limit_usd,
            pct=round(evaluation.windows["weekly"].pct, 2),
        ),
        monthly=WindowSpendOut(
            spend_usd=evaluation.windows["monthly"].spend_usd,
            limit_usd=evaluation.windows["monthly"].limit_usd,
            pct=round(evaluation.windows["monthly"].pct, 2),
        ),
        daily_limit_usd=evaluation.windows["daily"].limit_usd,
        weekly_limit_usd=evaluation.windows["weekly"].limit_usd,
        monthly_limit_usd=evaluation.windows["monthly"].limit_usd,
        alert_threshold_pct=budgets.get_budget(session).alert_threshold_pct,
        block_on_exceed=budgets.get_budget(session).block_on_exceed,
        override_until=budgets.get_budget(session).override_until,
    )


# --- routes ---


@router.get("/budgets", response_model=BudgetStatusOut)
def get_budget_status(session: Session = Depends(get_session)) -> BudgetStatusOut:
    return _status_out(session)


@router.put("/budgets", response_model=BudgetStatusOut)
def update_budget(
    body: BudgetUpdate,
    session: Session = Depends(get_session),
) -> BudgetStatusOut:
    budgets.upsert_budget(
        session,
        daily_limit_usd=body.daily_limit_usd,
        weekly_limit_usd=body.weekly_limit_usd,
        monthly_limit_usd=body.monthly_limit_usd,
        alert_threshold_pct=body.alert_threshold_pct,
        block_on_exceed=body.block_on_exceed,
    )
    log.info(
        "budgets.updated",
        daily=body.daily_limit_usd,
        weekly=body.weekly_limit_usd,
        monthly=body.monthly_limit_usd,
    )
    return _status_out(session)


@router.post("/budgets/override", response_model=BudgetStatusOut)
def set_override(
    body: OverrideRequest,
    session: Session = Depends(get_session),
) -> BudgetStatusOut:
    if body.until <= datetime.now(UTC):
        raise HTTPException(status_code=400, detail="override `until` must be in the future")
    budgets.set_override(session, body.until)
    log.info("budgets.override_set", until=body.until.isoformat())
    return _status_out(session)


@router.delete("/budgets/override", response_model=BudgetStatusOut)
def clear_override(session: Session = Depends(get_session)) -> BudgetStatusOut:
    budgets.set_override(session, None)
    log.info("budgets.override_cleared")
    return _status_out(session)


@router.get("/budgets/spend", response_model=list[SpendRowOut])
def list_spend(
    limit: int = Query(default=100, ge=1, le=1000),
    since: datetime | None = Query(default=None),
    session: Session = Depends(get_session),
) -> list[SpendRowOut]:
    rows = budgets.list_spend(session, since=since, limit=limit)
    return [
        SpendRowOut(
            id=r.id,
            run_id=r.run_id,
            conversation_id=r.conversation_id,
            provider_name=r.provider_name,
            model=r.model,
            prompt_tokens=r.prompt_tokens,
            completion_tokens=r.completion_tokens,
            total_tokens=r.total_tokens,
            cost_usd=r.cost_usd,
            ts=r.ts,
        )
        for r in rows
    ]
