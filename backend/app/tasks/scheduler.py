"""APScheduler engine for recurring tasks (Фаза 3b §1).

An ``AsyncIOScheduler`` runs inside the FastAPI process (started/stopped by the
app lifespan) and holds one job per enabled ``ScheduledTask``. Each job:

- fires ``service.fire_task`` inline, so ``max_instances=1`` really prevents a
  task from overlapping itself;
- coalesces bunched-up fire times into one run;
- honours the task's misfire policy (``run`` = execute however late, ``skip`` =
  drop a fire that is more than ``scheduler_misfire_grace_s`` seconds late).

**Job store:** jobs live in memory and are rebuilt from the ``scheduled_tasks``
table on every startup. The table — not an APScheduler jobstore — is the durable
source of truth, so schedules survive restarts without persisting pickled
triggers that could drift from the model.
"""

from __future__ import annotations

from datetime import UTC, datetime

from apscheduler.schedulers.asyncio import AsyncIOScheduler
from apscheduler.triggers.base import BaseTrigger
from apscheduler.triggers.cron import CronTrigger
from apscheduler.triggers.date import DateTrigger
from apscheduler.triggers.interval import IntervalTrigger
from sqlmodel import Session

from app.core.config import get_settings
from app.core.db import engine
from app.core.logging import get_logger
from app.models.task import (
    MISFIRE_RUN,
    TRIGGER_CRON,
    TRIGGER_DATE,
    TRIGGER_INTERVAL,
    ScheduledTask,
)
from app.tasks.cron import as_utc, resolve_timezone
from app.tasks.service import (
    compute_next_run,
    fire_task,
    list_tasks,
    record_skipped_run,
    refresh_next_run,
)

log = get_logger(__name__)

_scheduler: AsyncIOScheduler | None = None


def get_scheduler() -> AsyncIOScheduler | None:
    """The live scheduler, or None when it was never started."""
    return _scheduler


def is_running() -> bool:
    return _scheduler is not None and _scheduler.running


def job_id(task_id: int) -> str:
    """APScheduler job id for a task."""
    return f"task:{task_id}"


def build_trigger(task: ScheduledTask) -> BaseTrigger | None:
    """Translate a task's schedule into an APScheduler trigger."""
    tz = resolve_timezone(task.timezone)
    if task.trigger_type == TRIGGER_CRON and task.cron_expression:
        fields = task.cron_expression.split()
        if len(fields) == 6:
            second, minute, hour, dom, month, dow = fields
            return CronTrigger(
                second=second,
                minute=minute,
                hour=hour,
                day=dom,
                month=month,
                day_of_week=dow,
                timezone=tz,
            )
        return CronTrigger.from_crontab(task.cron_expression, timezone=tz)
    if task.trigger_type == TRIGGER_INTERVAL and task.interval_seconds:
        return IntervalTrigger(seconds=task.interval_seconds, timezone=tz)
    if task.trigger_type == TRIGGER_DATE and task.run_at:
        return DateTrigger(run_date=as_utc(task.run_at), timezone=tz)
    return None


def _misfire_grace(task: ScheduledTask) -> int | None:
    """Seconds a late fire may still run. None = run however late."""
    if task.misfire_policy == MISFIRE_RUN:
        return None
    return max(1, get_settings().scheduler_misfire_grace_s)


async def _run_job(task_id: int) -> None:
    """APScheduler job body: execute one fire of a task."""
    try:
        await fire_task(task_id, fire_time=datetime.now(UTC))
    except Exception as exc:  # a failing job must not kill the scheduler
        log.error("scheduler.job_failed", task_id=task_id, error=str(exc))


def sync_task_job(task: ScheduledTask) -> None:
    """Add, replace or remove the job for *task* to match its current state.

    A no-op when the scheduler isn't running (CLI usage, tests), so the service
    layer can call this unconditionally.
    """
    if _scheduler is None or not _scheduler.running or task.id is None:
        return
    jid = job_id(task.id)
    if not task.enabled:
        remove_task_job(task.id)
        return
    trigger = build_trigger(task)
    if trigger is None:
        log.warning("scheduler.no_trigger", task_id=task.id, trigger_type=task.trigger_type)
        remove_task_job(task.id)
        return
    _scheduler.add_job(
        _run_job,
        trigger=trigger,
        args=[task.id],
        id=jid,
        name=task.name,
        replace_existing=True,
        coalesce=True,
        max_instances=1,
        misfire_grace_time=_misfire_grace(task),
    )
    log.info(
        "scheduler.job_synced",
        task_id=task.id,
        trigger=task.trigger_type,
        next_run_at=str(task.next_run_at),
    )


def remove_task_job(task_id: int) -> None:
    """Drop a task's job if the scheduler holds one."""
    if _scheduler is None:
        return
    try:
        _scheduler.remove_job(job_id(task_id))
        log.info("scheduler.job_removed", task_id=task_id)
    except Exception:
        # JobLookupError (or a stopped scheduler) — nothing to remove.
        pass


def _catch_up(session: Session, task: ScheduledTask, *, now: datetime) -> None:
    """Handle a fire time missed while the process was down.

    ``misfire_policy=run`` executes the task once now; ``skip`` records a skipped
    run. Either way ``next_run_at`` moves forward so the task doesn't stay stuck
    in the past.
    """
    if task.next_run_at is None:
        return
    due = as_utc(task.next_run_at)
    if due > now:
        return
    if task.misfire_policy == MISFIRE_RUN:
        from app.tasks.service import schedule_task_execution

        log.info("scheduler.catch_up_run", task_id=task.id, missed_at=str(due))
        schedule_task_execution(session, task, fire_time=now)
    else:
        record_skipped_run(
            session,
            task,
            reason=f"missed fire time {due.isoformat()} (process was down)",
        )
    refresh_next_run(session, task)


def load_jobs(session: Session, *, catch_up: bool = True) -> int:
    """(Re)register jobs for every enabled task. Returns the job count."""
    now = datetime.now(UTC)
    count = 0
    for task in list_tasks(session, enabled=True):
        if catch_up:
            _catch_up(session, task, now=now)
        if not task.enabled:  # a one-shot may have been consumed by catch-up
            continue
        if task.next_run_at is None:
            task.next_run_at = compute_next_run(task)
            session.add(task)
            session.commit()
        sync_task_job(task)
        count += 1
    return count


async def start_scheduler() -> int:
    """Start the engine and load jobs from the database. Returns the job count.

    Returns 0 without starting anything when ``scheduler_enabled`` is False.
    Also registers internal maintenance jobs (memory lifecycle sweeps).
    """
    global _scheduler
    settings = get_settings()
    if not settings.scheduler_enabled:
        log.info("scheduler.disabled")
        return 0
    if _scheduler is not None and _scheduler.running:
        return len(_scheduler.get_jobs())

    _scheduler = AsyncIOScheduler(timezone=resolve_timezone(settings.scheduler_timezone))
    _scheduler.start()
    with Session(engine) as session:
        count = load_jobs(session)

    # Register internal maintenance: daily memory lifecycle sweep (decay, TTL,
    # validity, pending-expiry). Runs at 03:00 UTC to avoid user-active hours.
    _register_maintenance_jobs()

    log.info("scheduler.started", jobs=count, timezone=settings.scheduler_timezone)
    return count


def _register_maintenance_jobs() -> None:
    """Register built-in maintenance jobs (memory lifecycle)."""
    if _scheduler is None or not _scheduler.running:
        return
    _scheduler.add_job(
        _run_memory_maintenance,
        trigger=CronTrigger(hour=3, minute=0, timezone="UTC"),
        id="internal:memory_maintenance",
        name="Memory lifecycle maintenance",
        replace_existing=True,
        coalesce=True,
        max_instances=1,
    )
    log.info("scheduler.maintenance_registered")


async def _run_memory_maintenance() -> None:
    """Run all memory lifecycle sweeps (decay, TTL, validity, pending expiry)."""
    try:
        from app.memory.lifecycle import run_full_maintenance

        with Session(engine) as session:
            results = run_full_maintenance(session)
        log.info("scheduler.memory_maintenance_done", **results)
    except Exception as exc:
        log.error("scheduler.memory_maintenance_failed", error=str(exc))


async def shutdown_scheduler() -> None:
    """Stop the engine (without waiting for in-flight jobs to finish)."""
    global _scheduler
    if _scheduler is None:
        return
    try:
        _scheduler.shutdown(wait=False)
    except Exception as exc:
        log.warning("scheduler.shutdown_failed", error=str(exc))
    _scheduler = None
    log.info("scheduler.stopped")


def scheduler_status() -> dict:
    """Engine state for the API / UI."""
    settings = get_settings()
    jobs = []
    if _scheduler is not None and _scheduler.running:
        for job in _scheduler.get_jobs():
            jobs.append(
                {
                    "id": job.id,
                    "name": job.name,
                    "next_run_time": job.next_run_time.isoformat()
                    if job.next_run_time
                    else None,
                }
            )
    return {
        "enabled": settings.scheduler_enabled,
        "running": is_running(),
        "timezone": settings.scheduler_timezone,
        "max_concurrent_tasks": settings.scheduler_max_concurrent_tasks,
        "jobs": jobs,
    }
