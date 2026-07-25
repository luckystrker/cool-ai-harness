"""Plan tracking tools (Фаза 2 §1).

Provides the ``plan_step_update`` tool that the agent uses during build-mode
execution to mark plan steps as completed/failed/skipped. This updates the
persisted Plan/PlanStep rows and emits events so the frontend PlanCard
reflects progress in real-time.
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field
from sqlmodel import Session, select

from app.core.db import engine
from app.core.logging import get_logger
from app.models.plan import (
    PLAN_STATUS_APPROVED,
    PLAN_STATUS_COMPLETED,
    PLAN_STATUS_EXECUTING,
    PLAN_STATUS_FAILED,
    STEP_STATUS_COMPLETED,
    STEP_STATUS_FAILED,
    STEP_STATUS_SKIPPED,
    Plan,
    PlanStep,
)
from app.tools.base import ToolArgs, ToolResult, register_tool

log = get_logger(__name__)

_VALID_STEP_STATUSES = frozenset({STEP_STATUS_COMPLETED, STEP_STATUS_FAILED, STEP_STATUS_SKIPPED})
_TERMINAL_STEP_STATUSES = frozenset({STEP_STATUS_COMPLETED, STEP_STATUS_FAILED, STEP_STATUS_SKIPPED})


class PlanStepUpdateArgs(ToolArgs):
    """Arguments for the plan_step_update tool."""

    plan_id: int = Field(description="ID of the plan to update")
    position: int = Field(description="Position (0-based) of the step to update")
    status: Literal["completed", "failed", "skipped"] = Field(
        description="New status for the step"
    )
    summary: str = Field(default="", description="Brief summary of what was accomplished or why it failed")


async def _plan_step_update(
    plan_id: int, position: int, status: str, summary: str = ""
) -> ToolResult:
    """Update a plan step's status during build-mode execution."""
    if status not in _VALID_STEP_STATUSES:
        return ToolResult.err(f"Invalid status {status!r}. Must be one of: completed, failed, skipped")

    with Session(engine) as session:
        plan = session.get(Plan, plan_id)
        if plan is None:
            return ToolResult.err(f"Plan {plan_id} not found")

        # Find the step by position.
        step = session.exec(
            select(PlanStep)
            .where(PlanStep.plan_id == plan_id)
            .where(PlanStep.position == position)
        ).first()
        if step is None:
            return ToolResult.err(f"Step at position {position} not found in plan {plan_id}")

        # Update the step.
        step.status = status
        step.result_summary = summary or None
        session.add(step)

        # Auto-transition plan status: approved -> executing on first update.
        if plan.status == PLAN_STATUS_APPROVED:
            plan.status = PLAN_STATUS_EXECUTING

        # Update the denormalized steps JSON on the Plan row.
        all_steps = list(
            session.exec(
                select(PlanStep).where(PlanStep.plan_id == plan_id).order_by(PlanStep.position)  # type: ignore[arg-type]
            ).all()
        )
        plan.steps = [
            {
                "position": s.position,
                "title": s.title,
                "description": s.description,
                "status": s.status,
                "depends_on": s.depends_on,
                "tools": s.tools,
                "result_summary": s.result_summary,
            }
            for s in all_steps
        ]

        # Check if all steps are terminal -> finalize plan status.
        all_terminal = all(s.status in _TERMINAL_STEP_STATUSES for s in all_steps)
        if all_terminal:
            has_failed = any(s.status == STEP_STATUS_FAILED for s in all_steps)
            plan.status = PLAN_STATUS_FAILED if has_failed else PLAN_STATUS_COMPLETED

        session.add(plan)
        session.commit()

        log.info(
            "plan.step_updated",
            plan_id=plan_id,
            position=position,
            status=status,
            plan_status=plan.status,
        )

    return ToolResult.ok(
        f"Step {position + 1} marked as {status}."
        + (f" Summary: {summary}" if summary else "")
        + (f" Plan status: {plan.status}." if all_terminal else "")
    )


def register_plan_tools() -> None:
    """Register plan tracking tools. Idempotent."""
    register_tool(
        name="plan_step_update",
        description=(
            "Update the status of a plan step during execution. "
            "Call this after completing each step of the active plan to track progress. "
            "The plan_id and step positions are shown in the Active Plan section of your context."
        ),
        args_model=PlanStepUpdateArgs,
        func=_plan_step_update,
    )
