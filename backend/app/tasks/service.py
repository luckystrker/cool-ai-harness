"""Scheduled task service: CRUD, triggering, and durable execution (Фаза 3b §2, §3).

A task fires → a ``TaskRun`` is created with its own isolated conversation and
durable ``AgentRun`` (exactly like a subagent) → the shared
``run_conversation_turn`` runner drives the same agent loop the chat UI uses →
the result is recorded on the run and delivered to the task's channels.

Background runs are non-interactive: ``auto_approve=True`` turns "ask" tools
into "allow", so the loop never blocks on a human. The one exception is tools
with an external side effect (``send_external``): unless the task is explicitly
pre-approved, that capability is denied and the decision + reason are stored on
the ``TaskRun`` for auditing.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import Sequence
from datetime import UTC, datetime, timedelta

from sqlmodel import Session, col, select

from app.core.config import get_settings
from app.core.db import engine
from app.core.logging import get_logger
from app.models.task import (
    APPROVAL_ALLOW_ALL,
    APPROVAL_DENY_EXTERNAL,
    APPROVAL_POLICIES,
    MISFIRE_RUN,
    MISFIRE_SKIP,
    TASK_RUN_CANCELLED,
    TASK_RUN_COMPLETED,
    TASK_RUN_FAILED,
    TASK_RUN_QUEUED,
    TASK_RUN_RUNNING,
    TASK_RUN_SKIPPED,
    TERMINAL_TASK_RUN_STATUSES,
    TRIGGER_CRON,
    TRIGGER_DATE,
    TRIGGER_INTERVAL,
    TRIGGER_SOURCE_MANUAL,
    TRIGGER_SOURCE_SCHEDULE,
    TRIGGER_TYPES,
    ScheduledTask,
    TaskRun,
)
from app.tasks.cron import as_utc, in_quiet_hours, next_cron_run, validate_cron

log = get_logger(__name__)

# Fields a create/update accepts verbatim (no extra validation needed).
_PLAIN_FIELDS = frozenset(
    {
        "name",
        "description",
        "prompt",
        "workflow_type",
        "profile_id",
        "model",
        "tools_whitelist",
        "capability_policy",
        "working_directory",
        "delivery_channels",
        "delivery_config",
        "max_iterations",
        "max_cost_per_run",
        "timeout_s",
        "enabled",
    }
)

# Fields that change *when* a task fires — touching any of them recomputes
# next_run_at and reschedules the APScheduler job.
_SCHEDULE_FIELDS = frozenset(
    {"trigger_type", "cron_expression", "interval_seconds", "run_at", "timezone"}
)


class TaskRunRegistry:
    """In-memory registry of active task-run asyncio tasks (for cancellation).

    Mirrors ``app.agent.subagents.SubagentRegistry``: the DB tracks status, this
    tracks liveness so a cancel request can actually stop the coroutine.
    """

    def __init__(self) -> None:
        self._tasks: dict[int, asyncio.Task] = {}

    def register(self, task_run_id: int, task: asyncio.Task) -> None:
        self._tasks[task_run_id] = task

    def unregister(self, task_run_id: int) -> None:
        self._tasks.pop(task_run_id, None)

    def cancel(self, task_run_id: int) -> bool:
        task = self._tasks.get(task_run_id)
        if task is not None and not task.done():
            task.cancel()
            return True
        return False

    def is_active(self, task_run_id: int) -> bool:
        task = self._tasks.get(task_run_id)
        return task is not None and not task.done()

    @property
    def active_ids(self) -> list[int]:
        return [k for k, v in self._tasks.items() if not v.done()]


task_run_registry = TaskRunRegistry()

# One semaphore per event loop: asyncio primitives bind to the loop that first
# awaits them, and the test suite runs each test in a fresh loop.
_concurrency_guards: dict[int, asyncio.Semaphore] = {}


def _concurrency_guard() -> asyncio.Semaphore:
    """Global limit on simultaneously executing task runs."""
    loop = asyncio.get_running_loop()
    guard = _concurrency_guards.get(id(loop))
    if guard is None:
        limit = max(1, get_settings().scheduler_max_concurrent_tasks)
        guard = asyncio.Semaphore(limit)
        _concurrency_guards[id(loop)] = guard
    return guard


# --- Schedule validation & next-run computation ----------------------------


def validate_schedule(
    *,
    trigger_type: str,
    cron_expression: str | None,
    interval_seconds: int | None,
    run_at: datetime | None,
) -> None:
    """Validate a trigger definition. Raises ``ValueError`` when unusable."""
    if trigger_type not in TRIGGER_TYPES:
        raise ValueError(
            f"Unknown trigger_type {trigger_type!r} (expected one of {sorted(TRIGGER_TYPES)})"
        )
    if trigger_type == TRIGGER_CRON:
        if not cron_expression:
            raise ValueError("cron_expression is required for a cron trigger")
        validate_cron(cron_expression)
    elif trigger_type == TRIGGER_INTERVAL:
        if not interval_seconds or interval_seconds < 1:
            raise ValueError("interval_seconds must be a positive integer for an interval trigger")
    elif trigger_type == TRIGGER_DATE:
        if run_at is None:
            raise ValueError("run_at is required for a one-shot (date) trigger")


def compute_next_run(task: ScheduledTask, *, after: datetime | None = None) -> datetime | None:
    """Next fire time (UTC) for a task, or None when it will never fire again."""
    base = as_utc(after or datetime.now(UTC))
    if task.trigger_type == TRIGGER_CRON and task.cron_expression:
        try:
            return next_cron_run(task.cron_expression, timezone=task.timezone, after=base)
        except ValueError:
            return None
    if task.trigger_type == TRIGGER_INTERVAL and task.interval_seconds:
        return base + timedelta(seconds=task.interval_seconds)
    if task.trigger_type == TRIGGER_DATE and task.run_at:
        run_at = as_utc(task.run_at)
        return run_at if run_at > base else None
    return None


def refresh_next_run(session: Session, task: ScheduledTask, *, commit: bool = True) -> ScheduledTask:
    """Recompute and persist ``next_run_at`` for a task."""
    task.next_run_at = compute_next_run(task) if task.enabled else None
    task.updated_at = datetime.now(UTC)
    session.add(task)
    if commit:
        session.commit()
        session.refresh(task)
    return task


# --- CRUD -----------------------------------------------------------------


def create_task(
    session: Session,
    *,
    user_id: int,
    name: str,
    prompt: str,
    trigger_type: str = TRIGGER_CRON,
    cron_expression: str | None = None,
    interval_seconds: int | None = None,
    run_at: datetime | None = None,
    timezone: str | None = None,
    quiet_hours_start: str | None = None,
    quiet_hours_end: str | None = None,
    misfire_policy: str = MISFIRE_SKIP,
    description: str | None = None,
    workflow_type: str | None = None,
    profile_id: int | None = None,
    model: str | None = None,
    tools_whitelist: list[str] | None = None,
    capability_policy: dict | None = None,
    working_directory: str | None = None,
    approval_policy: str = APPROVAL_DENY_EXTERNAL,
    delivery_channels: list[str] | None = None,
    delivery_config: dict | None = None,
    max_iterations: int = 10,
    max_cost_per_run: float | None = None,
    timeout_s: float | None = None,
    enabled: bool = True,
) -> ScheduledTask:
    """Create a scheduled task. Raises ``ValueError`` on an invalid schedule."""
    validate_schedule(
        trigger_type=trigger_type,
        cron_expression=cron_expression,
        interval_seconds=interval_seconds,
        run_at=run_at,
    )
    if approval_policy not in APPROVAL_POLICIES:
        raise ValueError(
            f"Unknown approval_policy {approval_policy!r} "
            f"(expected one of {sorted(APPROVAL_POLICIES)})"
        )
    if misfire_policy not in (MISFIRE_SKIP, MISFIRE_RUN):
        raise ValueError(f"Unknown misfire_policy {misfire_policy!r}")

    task = ScheduledTask(
        user_id=user_id,
        name=name,
        description=description,
        trigger_type=trigger_type,
        cron_expression=validate_cron(cron_expression) if cron_expression else None,
        interval_seconds=interval_seconds,
        run_at=run_at,
        timezone=timezone or get_settings().scheduler_timezone,
        quiet_hours_start=quiet_hours_start,
        quiet_hours_end=quiet_hours_end,
        misfire_policy=misfire_policy,
        prompt=prompt,
        workflow_type=workflow_type,
        profile_id=profile_id,
        model=model,
        tools_whitelist=tools_whitelist,
        capability_policy=capability_policy,
        working_directory=working_directory,
        approval_policy=approval_policy,
        delivery_channels=delivery_channels,
        delivery_config=delivery_config,
        max_iterations=max_iterations,
        max_cost_per_run=max_cost_per_run,
        timeout_s=timeout_s,
        enabled=enabled,
    )
    task.next_run_at = compute_next_run(task) if enabled else None
    session.add(task)
    session.commit()
    session.refresh(task)
    log.info("task.created", task_id=task.id, name=task.name, trigger=task.trigger_type)
    _sync_scheduler(task)
    return task


def get_task(session: Session, task_id: int) -> ScheduledTask | None:
    return session.get(ScheduledTask, task_id)


def list_tasks(
    session: Session,
    *,
    user_id: int | None = None,
    enabled: bool | None = None,
) -> Sequence[ScheduledTask]:
    """Tasks, newest first, optionally filtered by owner / enabled state."""
    stmt = select(ScheduledTask).order_by(col(ScheduledTask.id).desc())
    if user_id is not None:
        stmt = stmt.where(ScheduledTask.user_id == user_id)
    if enabled is not None:
        stmt = stmt.where(ScheduledTask.enabled == enabled)
    return session.exec(stmt).all()


def update_task(session: Session, task_id: int, **fields) -> ScheduledTask | None:
    """Patch a task. Validates the schedule and reschedules the live job."""
    task = session.get(ScheduledTask, task_id)
    if task is None:
        return None

    schedule_touched = bool(_SCHEDULE_FIELDS & fields.keys()) or "enabled" in fields

    if fields.get("cron_expression"):
        fields["cron_expression"] = validate_cron(fields["cron_expression"])
    if "approval_policy" in fields and fields["approval_policy"] not in APPROVAL_POLICIES:
        raise ValueError(f"Unknown approval_policy {fields['approval_policy']!r}")
    if "misfire_policy" in fields and fields["misfire_policy"] not in (MISFIRE_SKIP, MISFIRE_RUN):
        raise ValueError(f"Unknown misfire_policy {fields['misfire_policy']!r}")

    for key, value in fields.items():
        if key in _PLAIN_FIELDS or key in _SCHEDULE_FIELDS or hasattr(task, key):
            setattr(task, key, value)

    validate_schedule(
        trigger_type=task.trigger_type,
        cron_expression=task.cron_expression,
        interval_seconds=task.interval_seconds,
        run_at=task.run_at,
    )

    if schedule_touched:
        task.next_run_at = compute_next_run(task) if task.enabled else None
    task.updated_at = datetime.now(UTC)
    session.add(task)
    session.commit()
    session.refresh(task)
    _sync_scheduler(task)
    return task


def delete_task(session: Session, task_id: int) -> bool:
    """Delete a task and its run history."""
    task = session.get(ScheduledTask, task_id)
    if task is None:
        return False
    for run in session.exec(select(TaskRun).where(TaskRun.task_id == task_id)).all():
        session.delete(run)
    session.delete(task)
    session.commit()
    _unsync_scheduler(task_id)
    log.info("task.deleted", task_id=task_id)
    return True


# --- Run records ----------------------------------------------------------


def get_task_run(session: Session, task_run_id: int) -> TaskRun | None:
    return session.get(TaskRun, task_run_id)


def list_task_runs(
    session: Session,
    *,
    task_id: int | None = None,
    status: str | None = None,
    unread_only: bool = False,
    limit: int = 50,
) -> Sequence[TaskRun]:
    """Runs, newest first."""
    stmt = select(TaskRun).order_by(col(TaskRun.id).desc())
    if task_id is not None:
        stmt = stmt.where(TaskRun.task_id == task_id)
    if status is not None:
        stmt = stmt.where(TaskRun.status == status)
    if unread_only:
        stmt = stmt.where(TaskRun.is_read == False)  # noqa: E712
    return session.exec(stmt.limit(limit)).all()


def mark_run_read(session: Session, task_run_id: int, *, is_read: bool = True) -> TaskRun | None:
    """Flip a run's inbox read state."""
    run = session.get(TaskRun, task_run_id)
    if run is None:
        return None
    run.is_read = is_read
    run.updated_at = datetime.now(UTC)
    session.add(run)
    session.commit()
    session.refresh(run)
    return run


def unread_count(session: Session, *, user_id: int | None = None) -> int:
    """Number of unread runs (notification badge)."""
    stmt = select(TaskRun).where(TaskRun.is_read == False)  # noqa: E712
    runs = session.exec(stmt).all()
    if user_id is None:
        return len(runs)
    owned = {
        t.id
        for t in session.exec(
            select(ScheduledTask).where(ScheduledTask.user_id == user_id)
        ).all()
    }
    return sum(1 for r in runs if r.task_id in owned)


def create_task_run(
    session: Session,
    task: ScheduledTask,
    *,
    trigger_source: str = TRIGGER_SOURCE_SCHEDULE,
) -> TaskRun:
    """Create an isolated conversation + durable AgentRun + TaskRun row."""
    from app.agent.service import append_message, create_conversation, create_run

    model = task.model or _default_model(session)
    conv = create_conversation(
        session,
        user_id=task.user_id,
        title=f"[Task] {task.name}",
        model=model,
        working_directory=task.working_directory,
        capability_policy=task.capability_policy,
        profile_id=task.profile_id,
    )
    # Hidden from the chat sidebar: a scheduled run is not a user conversation.
    conv.metadata_ = {**(conv.metadata_ or {}), "is_task": True, "task_id": task.id}
    session.add(conv)
    session.commit()
    session.refresh(conv)

    agent_run = create_run(
        session,
        conversation_id=conv.id,
        user_id=task.user_id,
        model=model,
        status="queued",
    )
    append_message(session, conversation_id=conv.id, role="user", content=task.prompt)

    run = TaskRun(
        task_id=task.id,
        conversation_id=conv.id,
        run_id=agent_run.id,
        status=TASK_RUN_QUEUED,
        trigger_source=trigger_source,
        prompt=task.prompt,
    )
    session.add(run)
    session.commit()
    session.refresh(run)
    return run


def record_skipped_run(
    session: Session,
    task: ScheduledTask,
    *,
    reason: str,
    trigger_source: str = TRIGGER_SOURCE_SCHEDULE,
) -> TaskRun:
    """Record a fire time that deliberately did not execute."""
    now = datetime.now(UTC)
    run = TaskRun(
        task_id=task.id,
        status=TASK_RUN_SKIPPED,
        trigger_source=trigger_source,
        prompt=task.prompt,
        skip_reason=reason,
        started_at=now,
        finished_at=now,
        # Skips are informational; they don't demand attention in the inbox.
        is_read=True,
    )
    session.add(run)
    task.last_run_at = now
    task.last_status = TASK_RUN_SKIPPED
    task.next_run_at = compute_next_run(task) if task.enabled else None
    session.add(task)
    session.commit()
    session.refresh(run)
    log.info("task.skipped", task_id=task.id, reason=reason)
    return run


def _default_model(session: Session) -> str:
    from app.agent.service import resolve_default_model

    return resolve_default_model(session) or "gpt-4o"


# --- Gating ---------------------------------------------------------------


def gate_run(
    session: Session,
    task: ScheduledTask,
    *,
    trigger_source: str,
    ignore_quiet_hours: bool = False,
    fire_time: datetime | None = None,
) -> TaskRun | None:
    """Pre-execution checks. Returns a skipped TaskRun when the run is blocked.

    Scheduled fires respect ``enabled`` and quiet hours; a manual/agent trigger
    is an explicit user action and only quiet hours can be opted into.
    """
    if trigger_source == TRIGGER_SOURCE_SCHEDULE and not task.enabled:
        return record_skipped_run(
            session, task, reason="task disabled", trigger_source=trigger_source
        )
    if not ignore_quiet_hours and in_quiet_hours(
        fire_time or datetime.now(UTC),
        timezone=task.timezone,
        start=task.quiet_hours_start,
        end=task.quiet_hours_end,
    ):
        return record_skipped_run(
            session,
            task,
            reason=f"quiet hours {task.quiet_hours_start} - {task.quiet_hours_end}",
            trigger_source=trigger_source,
        )
    return None


# --- Execution ------------------------------------------------------------


def _resolve_approval(task: ScheduledTask) -> tuple[dict[str, str], str]:
    """Effective capability policy + the recorded approval reason for a run."""
    policy: dict[str, str] = dict(task.capability_policy or {})
    if task.approval_policy == APPROVAL_ALLOW_ALL:
        reason = (
            "Pre-approved by the user: tools with an external side effect run "
            "without prompting in this background task."
        )
    else:
        policy.setdefault("send_external", "deny")
        reason = (
            "Background run without a human in the loop: tools with an external "
            "side effect (send_external) are denied. Set the task's approval "
            "policy to allow_all to pre-approve them."
        )
    return policy, reason


async def execute_task_run(task_run_id: int) -> str | None:
    """Drive one task run to completion. Returns its output.

    Designed to run as an asyncio task: it owns its DB session, enforces the
    task's timeout and cost limits, records the outcome, and delivers the result.
    """
    from app.agent import AgentLimits
    from app.agent.runners import run_conversation_turn
    from app.providers import get_provider_for_model

    settings = get_settings()

    with Session(engine) as session:
        run = session.get(TaskRun, task_run_id)
        if run is None:
            log.error("task_run.not_found", task_run_id=task_run_id)
            return None
        task = session.get(ScheduledTask, run.task_id)
        if task is None:
            _finalize_run(
                session, None, run, status=TASK_RUN_FAILED, error="Scheduled task no longer exists"
            )
            return None

        model = task.model or _default_model(session)
        cap_policy, approval_reason = _resolve_approval(task)

        run.status = TASK_RUN_RUNNING
        run.approval_policy = task.approval_policy
        run.approval_reason = approval_reason
        session.add(run)
        session.commit()
        log.info("task_run.started", task_id=task.id, task_run_id=run.id, model=model)

        provider = get_provider_for_model(model)
        limits = AgentLimits(
            max_iterations=task.max_iterations,
            max_cost_usd=task.max_cost_per_run,
        )
        timeout = task.timeout_s or settings.scheduler_task_timeout_s
        started = time.monotonic()

        async def _drive() -> tuple[str | None, dict | None, str | None]:
            """Consume the agent loop, returning (output, usage, error)."""
            output: str | None = None
            tokens: list[str] = []
            usage: dict | None = None
            error: str | None = None
            async for event in run_conversation_turn(
                session=session,
                conversation_id=run.conversation_id,
                provider=provider,
                model=model,
                user_input=None,  # prompt persisted by create_task_run
                tool_names=task.tools_whitelist,
                working_directory=task.working_directory,
                conversation_capability_policy=cap_policy,
                auto_approve=True,
                limits=limits,
                run_id=run.run_id,
                cancellable=True,
                profile_id=task.profile_id,
            ):
                if event.kind == "token":
                    tokens.append(event.payload.get("text", ""))
                elif event.kind == "message":
                    content = event.payload.get("content")
                    if content:
                        output = content
                    tokens.clear()
                elif event.kind == "error":
                    error = (
                        event.payload.get("detail")
                        or event.payload.get("message")
                        or "Unknown task error"
                    )
                elif event.kind == "finish":
                    usage = event.payload.get("usage")
            if output is None and tokens:
                output = "".join(tokens).strip() or None
            return output, usage, error

        try:
            async with _concurrency_guard():
                if timeout and timeout > 0:
                    output, usage, error = await asyncio.wait_for(_drive(), timeout=timeout)
                else:
                    output, usage, error = await _drive()
        except TimeoutError:
            _finalize_run(
                session,
                task,
                run,
                status=TASK_RUN_FAILED,
                error=f"Task run timed out after {timeout:.0f}s",
                duration_ms=_elapsed_ms(started),
            )
            task_run_registry.unregister(task_run_id)
            return None
        except asyncio.CancelledError:
            _finalize_run(
                session,
                task,
                run,
                status=TASK_RUN_CANCELLED,
                duration_ms=_elapsed_ms(started),
            )
            task_run_registry.unregister(task_run_id)
            raise
        except Exception as exc:
            _finalize_run(
                session,
                task,
                run,
                status=TASK_RUN_FAILED,
                error=str(exc),
                duration_ms=_elapsed_ms(started),
            )
            task_run_registry.unregister(task_run_id)
            return None

        status = TASK_RUN_FAILED if error else TASK_RUN_COMPLETED
        _finalize_run(
            session,
            task,
            run,
            status=status,
            output=output,
            usage=usage,
            error=error,
            duration_ms=_elapsed_ms(started),
        )
        task_run_registry.unregister(task_run_id)

        # Deliver the result (and failures — the inbox surfaces both).
        from app.tasks.delivery import deliver_task_run

        try:
            await deliver_task_run(session, task, run)
        except Exception as exc:  # delivery must not fail the run
            log.warning("task.delivery_failed", task_run_id=run.id, error=str(exc))

        return output


def _elapsed_ms(started: float) -> int:
    return int((time.monotonic() - started) * 1000)


def _finalize_run(
    session: Session,
    task: ScheduledTask | None,
    run: TaskRun,
    *,
    status: str,
    output: str | None = None,
    usage: dict | None = None,
    error: str | None = None,
    duration_ms: int | None = None,
) -> None:
    """Write the terminal state of a run and roll the task's counters forward."""
    now = datetime.now(UTC)
    run.status = status
    run.output = output
    run.error = error
    run.usage = usage
    run.duration_ms = duration_ms
    run.finished_at = now
    run.updated_at = now
    session.add(run)

    if task is not None:
        task.last_run_at = now
        task.last_status = status
        task.run_count += 1
        if status == TASK_RUN_FAILED:
            task.failure_count += 1
            ceiling = get_settings().scheduler_max_consecutive_failures
            if ceiling and task.failure_count >= ceiling:
                task.enabled = False
                log.warning(
                    "task.auto_disabled",
                    task_id=task.id,
                    failure_count=task.failure_count,
                )
        elif status == TASK_RUN_COMPLETED:
            task.failure_count = 0
        # A one-shot reminder is done after its single run.
        if task.trigger_type == TRIGGER_DATE and status in TERMINAL_TASK_RUN_STATUSES:
            task.enabled = False
        task.next_run_at = compute_next_run(task) if task.enabled else None
        task.updated_at = now
        session.add(task)

    session.commit()
    session.refresh(run)
    log.info(
        "task_run.finished",
        task_id=run.task_id,
        task_run_id=run.id,
        status=status,
        duration_ms=duration_ms,
    )
    if task is not None:
        _sync_scheduler(task)


def prepare_task_run(
    session: Session,
    task: ScheduledTask,
    *,
    trigger_source: str = TRIGGER_SOURCE_SCHEDULE,
    ignore_quiet_hours: bool = False,
    fire_time: datetime | None = None,
) -> TaskRun:
    """Apply the gates and create the run row (no execution yet).

    Returns a ``queued`` run when the task may execute, or a ``skipped`` run
    when a gate (disabled / quiet hours) blocked this fire time.
    """
    gated = gate_run(
        session,
        task,
        trigger_source=trigger_source,
        ignore_quiet_hours=ignore_quiet_hours,
        fire_time=fire_time,
    )
    if gated is not None:
        return gated
    return create_task_run(session, task, trigger_source=trigger_source)


def schedule_task_execution(
    session: Session,
    task: ScheduledTask,
    *,
    trigger_source: str = TRIGGER_SOURCE_SCHEDULE,
    ignore_quiet_hours: bool = False,
    fire_time: datetime | None = None,
) -> TaskRun:
    """Create a run and execute it in the background (fire-and-forget).

    Returns the ``TaskRun`` immediately: ``queued`` when execution was scheduled,
    ``skipped`` when a gate blocked it. Without a running event loop the run
    stays queued and can be driven explicitly via :func:`execute_task_run`.
    """
    run = prepare_task_run(
        session,
        task,
        trigger_source=trigger_source,
        ignore_quiet_hours=ignore_quiet_hours,
        fire_time=fire_time,
    )
    if run.status == TASK_RUN_SKIPPED:
        return run
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        log.debug("task_run.no_loop", task_run_id=run.id)
        return run
    async_task = loop.create_task(execute_task_run(run.id))
    task_run_registry.register(run.id, async_task)
    return run


async def fire_task(
    task_id: int,
    *,
    trigger_source: str = TRIGGER_SOURCE_SCHEDULE,
    ignore_quiet_hours: bool = False,
    fire_time: datetime | None = None,
) -> TaskRun | None:
    """Entry point for a scheduler fire: gate, create the run, execute it.

    Runs inline in the caller's coroutine so APScheduler's ``max_instances=1``
    actually prevents a task from overlapping itself. The coroutine registers
    itself for cancellation.
    """
    with Session(engine) as session:
        task = session.get(ScheduledTask, task_id)
        if task is None:
            log.warning("task.fire_unknown_task", task_id=task_id)
            return None
        run = prepare_task_run(
            session,
            task,
            trigger_source=trigger_source,
            ignore_quiet_hours=ignore_quiet_hours,
            fire_time=fire_time,
        )
        if run.status == TASK_RUN_SKIPPED:
            return run
        run_id = run.id

    current = asyncio.current_task()
    if current is not None:
        task_run_registry.register(run_id, current)
    await execute_task_run(run_id)
    with Session(engine) as session:
        return session.get(TaskRun, run_id)


async def run_task_now(
    session: Session,
    task_id: int,
    *,
    trigger_source: str = TRIGGER_SOURCE_MANUAL,
) -> TaskRun | None:
    """Run a task immediately and await its completion (manual / agent trigger).

    Quiet hours are ignored: the user explicitly asked for this run.
    """
    task = session.get(ScheduledTask, task_id)
    if task is None:
        return None
    run = prepare_task_run(
        session, task, trigger_source=trigger_source, ignore_quiet_hours=True
    )
    await execute_task_run(run.id)
    session.expire_all()
    return session.get(TaskRun, run.id)


def cancel_task_run(session: Session, task_run_id: int) -> bool:
    """Cancel an in-flight run. Returns True when cancellation was initiated."""
    run = session.get(TaskRun, task_run_id)
    if run is None or run.status in TERMINAL_TASK_RUN_STATUSES:
        return False
    if run.run_id is not None:
        from app.agent.runs import run_registry

        run_registry.cancel(run.run_id)
    if not task_run_registry.cancel(task_run_id):
        # Nothing live to cancel (already finished or never started) — mark it.
        now = datetime.now(UTC)
        run.status = TASK_RUN_CANCELLED
        run.finished_at = now
        run.updated_at = now
        session.add(run)
        session.commit()
    return True


# --- Scheduler hand-off ---------------------------------------------------
# Imported lazily so the service stays usable (and testable) without the
# APScheduler engine running.


def _sync_scheduler(task: ScheduledTask) -> None:
    """Re-register the task's job with the live scheduler, if one is running."""
    try:
        from app.tasks.scheduler import sync_task_job

        sync_task_job(task)
    except Exception as exc:
        log.debug("task.scheduler_sync_skipped", task_id=task.id, error=str(exc))


def _unsync_scheduler(task_id: int) -> None:
    try:
        from app.tasks.scheduler import remove_task_job

        remove_task_job(task_id)
    except Exception as exc:
        log.debug("task.scheduler_unsync_skipped", task_id=task_id, error=str(exc))
