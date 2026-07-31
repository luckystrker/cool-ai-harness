"""Recurring tasks / cron jobs (Фаза 3b).

Layout:
- ``cron``      — cron validation, next-run math, quiet hours, NL → cron parsing
- ``service``   — task CRUD, run creation, durable execution, delivery hand-off
- ``scheduler`` — the APScheduler engine (started by the app lifespan)
- ``delivery``  — result delivery to the configured channels
- ``templates`` — built-in recurring workflow presets
"""

from __future__ import annotations

from app.tasks.cron import (
    describe_cron,
    is_valid_cron,
    next_cron_run,
    next_cron_runs,
    parse_natural_schedule,
    validate_cron,
)
from app.tasks.service import (
    cancel_task_run,
    compute_next_run,
    create_task,
    create_task_run,
    delete_task,
    execute_task_run,
    fire_task,
    get_task,
    get_task_run,
    list_task_runs,
    list_tasks,
    mark_run_read,
    prepare_task_run,
    run_task_now,
    schedule_task_execution,
    unread_count,
    update_task,
)
from app.tasks.templates import TASK_TEMPLATES, get_template, list_templates

__all__ = [
    "TASK_TEMPLATES",
    "cancel_task_run",
    "compute_next_run",
    "create_task",
    "create_task_run",
    "delete_task",
    "describe_cron",
    "execute_task_run",
    "fire_task",
    "get_task",
    "get_task_run",
    "get_template",
    "is_valid_cron",
    "list_task_runs",
    "list_tasks",
    "list_templates",
    "mark_run_read",
    "next_cron_run",
    "next_cron_runs",
    "parse_natural_schedule",
    "prepare_task_run",
    "run_task_now",
    "schedule_task_execution",
    "unread_count",
    "update_task",
    "validate_cron",
]
