"""Recurring task routes: CRUD, manual trigger, run history, inbox (Фаза 3b).

Mounted at ``/api/tasks``. The scheduler engine itself is started by the app
lifespan; these routes only read/write task definitions and runs, and ask the
service layer to fire a run on demand.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field
from sqlmodel import Session

from app.agent.service import get_or_create_default_user, list_messages
from app.core.db import get_session
from app.models.task import (
    APPROVAL_DENY_EXTERNAL,
    MISFIRE_SKIP,
    TRIGGER_CRON,
    TRIGGER_SOURCE_MANUAL,
    ScheduledTask,
    TaskRun,
)
from app.tasks.cron import (
    as_utc,
    describe_cron,
    is_valid_cron,
    next_cron_runs,
    parse_natural_schedule,
)
from app.tasks.service import (
    cancel_task_run,
    create_task,
    delete_task,
    get_task,
    get_task_run,
    list_task_runs,
    list_tasks,
    mark_run_read,
    schedule_task_execution,
    unread_count,
    update_task,
)
from app.tasks.templates import get_template, list_templates

router = APIRouter(prefix="/tasks", tags=["tasks"])


# --- Schemas --------------------------------------------------------------


class TaskCreate(BaseModel):
    name: str
    prompt: str | None = Field(
        default=None, description="Required unless a template supplies it"
    )
    description: str | None = None
    # Schedule: cron (default), interval or one-shot date.
    trigger_type: str = TRIGGER_CRON
    cron_expression: str | None = None
    interval_seconds: int | None = None
    run_at: datetime | None = None
    timezone: str | None = None
    quiet_hours_start: str | None = None
    quiet_hours_end: str | None = None
    misfire_policy: str = MISFIRE_SKIP
    # Execution config.
    template: str | None = Field(default=None, description="Built-in workflow template slug")
    profile_id: int | None = None
    model: str | None = None
    tools_whitelist: list[str] | None = None
    capability_policy: dict[str, str] | None = None
    working_directory: str | None = None
    approval_policy: str = APPROVAL_DENY_EXTERNAL
    delivery_channels: list[str] | None = None
    delivery_config: dict[str, Any] | None = None
    max_iterations: int = 10
    max_cost_per_run: float | None = None
    timeout_s: float | None = None
    enabled: bool = True


class TaskUpdate(BaseModel):
    name: str | None = None
    prompt: str | None = None
    description: str | None = None
    trigger_type: str | None = None
    cron_expression: str | None = None
    interval_seconds: int | None = None
    run_at: datetime | None = None
    timezone: str | None = None
    quiet_hours_start: str | None = None
    quiet_hours_end: str | None = None
    misfire_policy: str | None = None
    profile_id: int | None = None
    model: str | None = None
    tools_whitelist: list[str] | None = None
    capability_policy: dict[str, str] | None = None
    working_directory: str | None = None
    approval_policy: str | None = None
    delivery_channels: list[str] | None = None
    delivery_config: dict[str, Any] | None = None
    max_iterations: int | None = None
    max_cost_per_run: float | None = None
    timeout_s: float | None = None
    enabled: bool | None = None


class TaskOut(BaseModel):
    id: int
    user_id: int
    name: str
    description: str | None = None
    trigger_type: str
    cron_expression: str | None = None
    interval_seconds: int | None = None
    run_at: datetime | None = None
    timezone: str
    quiet_hours_start: str | None = None
    quiet_hours_end: str | None = None
    misfire_policy: str
    prompt: str
    workflow_type: str | None = None
    profile_id: int | None = None
    model: str | None = None
    tools_whitelist: list[str] | None = None
    capability_policy: dict[str, Any] | None = None
    working_directory: str | None = None
    approval_policy: str
    delivery_channels: list[str] | None = None
    delivery_config: dict[str, Any] | None = None
    max_iterations: int
    max_cost_per_run: float | None = None
    timeout_s: float | None = None
    enabled: bool
    next_run_at: datetime | None = None
    last_run_at: datetime | None = None
    last_status: str | None = None
    run_count: int
    failure_count: int
    created_at: datetime
    updated_at: datetime
    # Derived, for the UI: human-readable schedule + upcoming fire times (UTC).
    schedule_description: str | None = None
    next_runs: list[datetime] = []


class TaskRunOut(BaseModel):
    id: int
    task_id: int
    conversation_id: int | None = None
    run_id: int | None = None
    status: str
    trigger_source: str
    prompt: str
    output: str | None = None
    error: str | None = None
    skip_reason: str | None = None
    approval_policy: str | None = None
    approval_reason: str | None = None
    usage: dict[str, Any] | None = None
    duration_ms: int | None = None
    delivery_status: dict[str, Any] | None = None
    delivered_at: datetime | None = None
    is_read: bool
    started_at: datetime
    finished_at: datetime | None = None
    created_at: datetime
    updated_at: datetime


class TaskRunDetail(TaskRunOut):
    """A run plus the messages of its isolated conversation."""

    messages: list[dict[str, Any]] = []


class ParseCronRequest(BaseModel):
    text: str


class ParseCronResponse(BaseModel):
    cron_expression: str | None = None
    description: str | None = None
    next_runs: list[datetime] = []
    detail: str | None = None


class InboxResponse(BaseModel):
    unread_count: int
    runs: list[TaskRunOut] = []


class ReadRequest(BaseModel):
    is_read: bool = True


# --- Mappers --------------------------------------------------------------

# Datetime columns read back from SQLite are naive; serializing them without an
# offset would make the browser read them as local time. Stamp them as UTC (the
# timezone they were written in) so the UI can convert to local reliably.
_TASK_DATETIME_FIELDS = ("run_at", "next_run_at", "last_run_at", "created_at", "updated_at")
_RUN_DATETIME_FIELDS = (
    "delivered_at",
    "started_at",
    "finished_at",
    "created_at",
    "updated_at",
)


def _stamp_utc(data: dict[str, Any], fields: tuple[str, ...]) -> dict[str, Any]:
    for field_name in fields:
        value = data.get(field_name)
        if isinstance(value, datetime):
            data[field_name] = as_utc(value)
    return data


def _task_to_out(task: ScheduledTask) -> TaskOut:
    upcoming: list[datetime] = []
    description: str | None = None
    if task.cron_expression and is_valid_cron(task.cron_expression):
        description = describe_cron(task.cron_expression)
        upcoming = next_cron_runs(task.cron_expression, timezone=task.timezone, count=3)
    return TaskOut(
        **_stamp_utc(task.model_dump(), _TASK_DATETIME_FIELDS),
        schedule_description=description,
        next_runs=upcoming,
    )


def _run_to_out(run: TaskRun) -> TaskRunOut:
    return TaskRunOut(**_stamp_utc(run.model_dump(), _RUN_DATETIME_FIELDS))


# --- Static sub-routes (must precede /{task_id}) --------------------------


@router.get("/templates")
def get_task_templates():
    """Built-in recurring workflow templates (Фаза 3b §5)."""
    return list_templates()


@router.get("/scheduler")
def get_scheduler_state():
    """Scheduler engine state: enabled/running, timezone, registered jobs."""
    from app.tasks.scheduler import scheduler_status

    return scheduler_status()


@router.post("/parse-cron", response_model=ParseCronResponse)
def parse_cron_endpoint(body: ParseCronRequest):
    """Translate a natural-language schedule (or validate a cron expression)."""
    text = (body.text or "").strip()
    if is_valid_cron(text):
        return ParseCronResponse(
            cron_expression=text,
            description=describe_cron(text),
            next_runs=next_cron_runs(text, count=3),
        )
    cron = parse_natural_schedule(text)
    if cron is None:
        return ParseCronResponse(detail=f"Could not interpret {text!r} as a schedule")
    return ParseCronResponse(
        cron_expression=cron,
        description=describe_cron(cron),
        next_runs=next_cron_runs(cron, count=3),
    )


@router.get("/inbox", response_model=InboxResponse)
def get_inbox(
    limit: int = Query(default=30, ge=1, le=200),
    unread_only: bool = Query(default=False),
    session: Session = Depends(get_session),
):
    """Unified feed of task results and failures (notification center)."""
    runs = list_task_runs(session, unread_only=unread_only, limit=limit)
    return InboxResponse(
        unread_count=unread_count(session),
        runs=[_run_to_out(r) for r in runs],
    )


@router.get("/runs", response_model=list[TaskRunOut])
def list_all_runs(
    task_id: int | None = Query(default=None),
    status: str | None = Query(default=None),
    limit: int = Query(default=50, ge=1, le=200),
    session: Session = Depends(get_session),
):
    """Run history across tasks (newest first)."""
    runs = list_task_runs(session, task_id=task_id, status=status, limit=limit)
    return [_run_to_out(r) for r in runs]


@router.get("/runs/{task_run_id}", response_model=TaskRunDetail)
def get_run(task_run_id: int, session: Session = Depends(get_session)):
    """One run plus the transcript of its isolated conversation."""
    run = get_task_run(session, task_run_id)
    if run is None:
        raise HTTPException(status_code=404, detail="Task run not found")
    messages: list[dict[str, Any]] = []
    if run.conversation_id is not None:
        messages = [
            {
                "id": m.id,
                "role": m.role,
                "content": m.content,
                "tool_calls": m.tool_calls,
                "tool_result": m.tool_result,
                "created_at": m.created_at,
            }
            for m in list_messages(session, run.conversation_id)
        ]
    return TaskRunDetail(**_run_to_out(run).model_dump(), messages=messages)


@router.post("/runs/{task_run_id}/read", response_model=TaskRunOut)
def set_run_read(
    task_run_id: int, body: ReadRequest, session: Session = Depends(get_session)
):
    """Mark a run read/unread in the inbox."""
    run = mark_run_read(session, task_run_id, is_read=body.is_read)
    if run is None:
        raise HTTPException(status_code=404, detail="Task run not found")
    return _run_to_out(run)


@router.post("/runs/{task_run_id}/cancel")
def cancel_run(task_run_id: int, session: Session = Depends(get_session)):
    """Cancel an in-flight run."""
    run = get_task_run(session, task_run_id)
    if run is None:
        raise HTTPException(status_code=404, detail="Task run not found")
    cancelled = cancel_task_run(session, task_run_id)
    return {"task_run_id": task_run_id, "cancelled": cancelled}


# --- Task CRUD ------------------------------------------------------------


@router.get("", response_model=list[TaskOut])
def list_tasks_endpoint(
    enabled: bool | None = Query(default=None),
    session: Session = Depends(get_session),
):
    """List scheduled tasks (newest first)."""
    return [_task_to_out(t) for t in list_tasks(session, enabled=enabled)]


@router.post("", response_model=TaskOut, status_code=201)
def create_task_endpoint(body: TaskCreate, session: Session = Depends(get_session)):
    """Create a scheduled task, optionally seeded from a workflow template."""
    preset = get_template(body.template) if body.template else None
    if body.template and preset is None:
        raise HTTPException(status_code=404, detail=f"Unknown template {body.template!r}")

    prompt = body.prompt or (preset.prompt if preset else None)
    if not prompt:
        raise HTTPException(status_code=422, detail="prompt is required")

    cron_expression = body.cron_expression or (preset.cron_expression if preset else None)
    user = get_or_create_default_user(session)
    assert user.id is not None
    try:
        task = create_task(
            session,
            user_id=user.id,
            name=body.name,
            prompt=prompt,
            description=body.description,
            trigger_type=body.trigger_type,
            cron_expression=cron_expression,
            interval_seconds=body.interval_seconds,
            run_at=body.run_at,
            timezone=body.timezone,
            quiet_hours_start=body.quiet_hours_start,
            quiet_hours_end=body.quiet_hours_end,
            misfire_policy=body.misfire_policy,
            workflow_type=preset.slug if preset else None,
            profile_id=body.profile_id,
            model=body.model,
            tools_whitelist=body.tools_whitelist
            or (preset.tools_whitelist if preset else None),
            capability_policy=body.capability_policy,
            working_directory=body.working_directory,
            approval_policy=body.approval_policy,
            delivery_channels=body.delivery_channels
            or (list(preset.delivery_channels) if preset else None),
            delivery_config=body.delivery_config,
            max_iterations=body.max_iterations,
            max_cost_per_run=body.max_cost_per_run,
            timeout_s=body.timeout_s,
            enabled=body.enabled,
        )
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    return _task_to_out(task)


@router.get("/{task_id}", response_model=TaskOut)
def get_task_endpoint(task_id: int, session: Session = Depends(get_session)):
    """Get one task."""
    task = get_task(session, task_id)
    if task is None:
        raise HTTPException(status_code=404, detail="Task not found")
    return _task_to_out(task)


@router.put("/{task_id}", response_model=TaskOut)
def update_task_endpoint(
    task_id: int, body: TaskUpdate, session: Session = Depends(get_session)
):
    """Update a task (only the provided fields)."""
    fields = body.model_dump(exclude_unset=True)
    if not fields:
        raise HTTPException(status_code=422, detail="No fields to update")
    try:
        task = update_task(session, task_id, **fields)
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    if task is None:
        raise HTTPException(status_code=404, detail="Task not found")
    return _task_to_out(task)


@router.delete("/{task_id}", status_code=204)
def delete_task_endpoint(task_id: int, session: Session = Depends(get_session)):
    """Delete a task and its run history."""
    if not delete_task(session, task_id):
        raise HTTPException(status_code=404, detail="Task not found")


@router.post("/{task_id}/run", response_model=TaskRunOut, status_code=202)
async def run_task_endpoint(task_id: int, session: Session = Depends(get_session)):
    """Trigger a task immediately (ignores quiet hours and the enabled flag).

    Returns the queued run right away; the agent loop continues in the
    background and the run row is updated as it progresses.

    This must be ``async def``: the background execution is spawned with
    ``asyncio.create_task`` on the running loop. A sync endpoint runs in a
    threadpool with no event loop, so the spawn would silently no-op and the
    run would stay queued forever.
    """
    task = get_task(session, task_id)
    if task is None:
        raise HTTPException(status_code=404, detail="Task not found")
    run = schedule_task_execution(
        session,
        task,
        trigger_source=TRIGGER_SOURCE_MANUAL,
        ignore_quiet_hours=True,
    )
    return _run_to_out(run)


@router.get("/{task_id}/runs", response_model=list[TaskRunOut])
def list_task_runs_endpoint(
    task_id: int,
    limit: int = Query(default=50, ge=1, le=200),
    session: Session = Depends(get_session),
):
    """Run history for one task."""
    task = get_task(session, task_id)
    if task is None:
        raise HTTPException(status_code=404, detail="Task not found")
    return [_run_to_out(r) for r in list_task_runs(session, task_id=task_id, limit=limit)]
