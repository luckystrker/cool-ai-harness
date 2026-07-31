"""Schedule tools: let the agent manage recurring tasks itself (Фаза 3b §4).

With these registered, "каждый понедельник в 9 утра присылай мне дайджест по
теме X" is handled end-to-end inside a chat turn: the agent parses the phrase
into a cron expression and creates the task. Tools mirror the REST API:
``create_task`` / ``list_tasks`` / ``update_task`` / ``delete_task`` /
``run_task_now`` / ``parse_cron``.
"""

from __future__ import annotations

from typing import Any

from pydantic import Field

from app.tools.base import ToolArgs, ToolResult, register_tool


class CreateTaskArgs(ToolArgs):
    """Arguments for the create_task tool."""

    name: str = Field(description="Short human-readable task name, e.g. 'AI news digest'.")
    prompt: str | None = Field(
        default=None,
        description="Instructions executed on every run. Optional when 'template' is given.",
    )
    schedule: str | None = Field(
        default=None,
        description=(
            "When to run, either a 5-field cron expression ('0 9 * * 1') or a "
            "natural-language phrase ('каждый день в 8 вечера', 'every weekday at 7:30')."
        ),
    )
    template: str | None = Field(
        default=None,
        description=(
            "Optional built-in workflow template slug: news-digest, code-review, "
            "memory-review, health-check. Fills in prompt/schedule/tools."
        ),
    )
    timezone: str | None = Field(
        default=None,
        description="IANA timezone the schedule is interpreted in (e.g. 'Europe/Berlin').",
    )
    model: str | None = Field(default=None, description="Model override for the task's runs.")
    tools: list[str] | None = Field(
        default=None,
        description="Tool whitelist for the task's runs. Omit to allow all tools.",
    )
    delivery_channels: list[str] | None = Field(
        default=None,
        description="Where results go: 'ui' (default) and/or 'webhook'.",
    )
    quiet_hours_start: str | None = Field(
        default=None, description="Start of the do-not-run window, 'HH:MM' local time."
    )
    quiet_hours_end: str | None = Field(
        default=None, description="End of the do-not-run window, 'HH:MM' local time."
    )
    enabled: bool = Field(default=True, description="Whether the task starts out enabled.")


class ListTasksArgs(ToolArgs):
    """Arguments for the list_tasks tool."""

    enabled_only: bool = Field(
        default=False, description="Only list enabled tasks (default: list all)."
    )


class UpdateTaskArgs(ToolArgs):
    """Arguments for the update_task tool."""

    task_id: int = Field(description="Id of the task to update.")
    name: str | None = None
    prompt: str | None = None
    schedule: str | None = Field(
        default=None, description="New cron expression or natural-language schedule."
    )
    timezone: str | None = None
    model: str | None = None
    tools: list[str] | None = None
    delivery_channels: list[str] | None = None
    enabled: bool | None = Field(default=None, description="Enable or pause the task.")


class TaskIdArgs(ToolArgs):
    """Arguments for tools that only need a task id."""

    task_id: int = Field(description="Id of the task.")


class ParseCronArgs(ToolArgs):
    """Arguments for the parse_cron tool."""

    text: str = Field(
        description="Natural-language schedule, e.g. 'каждый вторник в 18:30' or 'every 30 minutes'."
    )


def _resolve_schedule(text: str | None) -> tuple[str | None, str | None]:
    """Turn cron-or-prose into (cron_expression, error)."""
    from app.tasks.cron import is_valid_cron, parse_natural_schedule, validate_cron

    if not text:
        return None, None
    if is_valid_cron(text):
        return validate_cron(text), None
    cron = parse_natural_schedule(text)
    if cron is None:
        return None, (
            f"Could not interpret the schedule {text!r}. Provide a 5-field cron "
            "expression (minute hour day-of-month month day-of-week) instead."
        )
    return cron, None


def _task_summary(task: Any) -> dict[str, Any]:
    from app.tasks.cron import describe_cron

    return {
        "id": task.id,
        "name": task.name,
        "trigger_type": task.trigger_type,
        "cron_expression": task.cron_expression,
        "schedule": describe_cron(task.cron_expression) if task.cron_expression else None,
        "timezone": task.timezone,
        "enabled": task.enabled,
        "next_run_at": task.next_run_at.isoformat() if task.next_run_at else None,
        "last_run_at": task.last_run_at.isoformat() if task.last_run_at else None,
        "last_status": task.last_status,
        "delivery_channels": task.delivery_channels or ["ui"],
    }


async def _create_task(
    name: str,
    prompt: str | None = None,
    schedule: str | None = None,
    template: str | None = None,
    timezone: str | None = None,
    model: str | None = None,
    tools: list[str] | None = None,
    delivery_channels: list[str] | None = None,
    quiet_hours_start: str | None = None,
    quiet_hours_end: str | None = None,
    enabled: bool = True,
) -> ToolResult:
    """Create a recurring task on the user's behalf."""
    from sqlmodel import Session

    from app.agent.service import get_or_create_default_user
    from app.core.db import engine
    from app.tasks.cron import describe_cron
    from app.tasks.service import create_task
    from app.tasks.templates import get_template
    from app.tools.context import get_run_context

    preset = get_template(template) if template else None
    if template and preset is None:
        return ToolResult.err(
            f"Unknown template {template!r}. Available: news-digest, code-review, "
            "memory-review, health-check."
        )

    effective_prompt = prompt or (preset.prompt if preset else None)
    if not effective_prompt:
        return ToolResult.err("A task needs a prompt (or a template that provides one).")

    cron, error = _resolve_schedule(schedule)
    if error:
        return ToolResult.err(error)
    if cron is None:
        cron = preset.cron_expression if preset else None
    if cron is None:
        return ToolResult.err(
            "A task needs a schedule: pass a cron expression or a phrase like "
            "'every day at 8pm'."
        )

    ctx = get_run_context()
    workdir = str(ctx.workdir) if ctx and ctx.workdir else None

    with Session(engine) as session:
        user = get_or_create_default_user(session)
        assert user.id is not None
        try:
            task = create_task(
                session,
                user_id=user.id,
                name=name,
                prompt=effective_prompt,
                cron_expression=cron,
                timezone=timezone,
                quiet_hours_start=quiet_hours_start,
                quiet_hours_end=quiet_hours_end,
                workflow_type=preset.slug if preset else None,
                model=model,
                tools_whitelist=tools or (preset.tools_whitelist if preset else None),
                delivery_channels=delivery_channels
                or (list(preset.delivery_channels) if preset else None),
                working_directory=workdir,
                max_iterations=preset.max_iterations if preset else 10,
                enabled=enabled,
            )
        except ValueError as exc:
            return ToolResult.err(str(exc))
        return ToolResult.ok(
            {
                "created": _task_summary(task),
                "schedule_description": describe_cron(cron),
            },
            task_id=task.id,
        )


async def _list_tasks(enabled_only: bool = False) -> ToolResult:
    """List the user's recurring tasks."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.tasks.service import list_tasks

    with Session(engine) as session:
        tasks = list_tasks(session, enabled=True if enabled_only else None)
        return ToolResult.ok([_task_summary(t) for t in tasks], count=len(tasks))


async def _update_task(
    task_id: int,
    name: str | None = None,
    prompt: str | None = None,
    schedule: str | None = None,
    timezone: str | None = None,
    model: str | None = None,
    tools: list[str] | None = None,
    delivery_channels: list[str] | None = None,
    enabled: bool | None = None,
) -> ToolResult:
    """Update an existing recurring task."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.tasks.service import update_task

    fields: dict[str, Any] = {}
    if name is not None:
        fields["name"] = name
    if prompt is not None:
        fields["prompt"] = prompt
    if timezone is not None:
        fields["timezone"] = timezone
    if model is not None:
        fields["model"] = model
    if tools is not None:
        fields["tools_whitelist"] = tools
    if delivery_channels is not None:
        fields["delivery_channels"] = delivery_channels
    if enabled is not None:
        fields["enabled"] = enabled
    if schedule is not None:
        cron, error = _resolve_schedule(schedule)
        if error:
            return ToolResult.err(error)
        fields["cron_expression"] = cron
        fields["trigger_type"] = "cron"

    if not fields:
        return ToolResult.err("Nothing to update: pass at least one field.")

    with Session(engine) as session:
        try:
            task = update_task(session, task_id, **fields)
        except ValueError as exc:
            return ToolResult.err(str(exc))
        if task is None:
            return ToolResult.err(f"Task {task_id} not found.")
        return ToolResult.ok({"updated": _task_summary(task)}, task_id=task_id)


async def _delete_task(task_id: int) -> ToolResult:
    """Delete a recurring task and its run history."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.tasks.service import delete_task

    with Session(engine) as session:
        if not delete_task(session, task_id):
            return ToolResult.err(f"Task {task_id} not found.")
        return ToolResult.ok({"deleted": task_id})


async def _run_task_now(task_id: int) -> ToolResult:
    """Run a task immediately and return its output."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.tasks.service import run_task_now

    with Session(engine) as session:
        run = await run_task_now(session, task_id)
        if run is None:
            return ToolResult.err(f"Task {task_id} not found.")
        if run.error:
            return ToolResult.err(f"Task run failed: {run.error}", task_run_id=run.id)
        return ToolResult.ok(
            {
                "task_run_id": run.id,
                "status": run.status,
                "output": run.output,
                "duration_ms": run.duration_ms,
            },
            task_run_id=run.id,
        )


async def _parse_cron(text: str) -> ToolResult:
    """Translate a natural-language schedule into a cron expression."""
    from app.tasks.cron import describe_cron, next_cron_runs, parse_natural_schedule

    cron = parse_natural_schedule(text)
    if cron is None:
        return ToolResult.err(
            f"Could not interpret {text!r} as a schedule. Try phrasings like "
            "'every day at 8pm', 'каждый вторник в 18:30', 'every 30 minutes'."
        )
    upcoming = [dt.isoformat() for dt in next_cron_runs(cron, count=3)]
    return ToolResult.ok(
        {
            "cron_expression": cron,
            "description": describe_cron(cron),
            "next_runs_utc": upcoming,
        }
    )


def register_task_tools() -> None:
    """Register the schedule-management tools. Idempotent."""
    register_tool(
        name="create_task",
        description=(
            "Create a recurring (cron) task that runs a prompt on a schedule and "
            "delivers the result. Use when the user asks for something to happen "
            "regularly ('every Monday at 9am send me a digest'). The schedule may "
            "be a cron expression or a natural-language phrase."
        ),
        args_model=CreateTaskArgs,
        func=_create_task,
    )
    register_tool(
        name="list_tasks",
        description=(
            "List the user's recurring tasks with their schedule, next run time "
            "and last status."
        ),
        args_model=ListTasksArgs,
        func=_list_tasks,
    )
    register_tool(
        name="update_task",
        description=(
            "Update a recurring task: rename it, change its prompt, schedule, "
            "model, tools or delivery channels, or pause/resume it."
        ),
        args_model=UpdateTaskArgs,
        func=_update_task,
    )
    register_tool(
        name="delete_task",
        description="Delete a recurring task and its run history.",
        args_model=TaskIdArgs,
        func=_delete_task,
        dangerous=True,
    )
    register_tool(
        name="run_task_now",
        description=(
            "Run a recurring task immediately (ignoring its schedule and quiet "
            "hours) and return its output."
        ),
        args_model=TaskIdArgs,
        func=_run_task_now,
    )
    register_tool(
        name="parse_cron",
        description=(
            "Translate a natural-language schedule ('каждый день в 8 вечера', "
            "'every weekday at 7:30') into a 5-field cron expression plus the "
            "next few run times."
        ),
        args_model=ParseCronArgs,
        func=_parse_cron,
    )
