"""Tests for recurring tasks / cron jobs (Фаза 3b).

Covers: cron parsing/validation, natural-language schedules, quiet hours, task
CRUD, run isolation, gating (disabled / quiet hours), execution with the
ScriptedProvider, timeouts, approval policy, delivery + dedup, the inbox, the
agent-facing schedule tools, the APScheduler engine, and the REST API.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session

from app.core.db import engine
from app.models import Conversation
from app.models.task import (
    APPROVAL_ALLOW_ALL,
    APPROVAL_DENY_EXTERNAL,
    MISFIRE_RUN,
    TASK_RUN_COMPLETED,
    TASK_RUN_FAILED,
    TASK_RUN_QUEUED,
    TASK_RUN_SKIPPED,
    TRIGGER_DATE,
    TRIGGER_INTERVAL,
    TRIGGER_SOURCE_MANUAL,
    ScheduledTask,
)
from app.tasks.cron import (
    as_utc,
    describe_cron,
    in_quiet_hours,
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
    get_task,
    get_task_run,
    list_task_runs,
    list_tasks,
    mark_run_read,
    prepare_task_run,
    run_task_now,
    unread_count,
    update_task,
)


@pytest.fixture(autouse=True)
def _seed_user():
    """Ensure tables exist and a default user is present for FK constraints."""
    from app.agent.service import get_or_create_default_user
    from app.core.db import init_db

    init_db()
    with Session(engine) as session:
        get_or_create_default_user(session)


@pytest.fixture
def user_id() -> int:
    from app.agent.service import get_or_create_default_user

    with Session(engine) as session:
        return get_or_create_default_user(session).id


@pytest.fixture
def scripted(monkeypatch):
    """A ScriptedProvider wired into the task executor's provider lookup."""
    from tests.conftest import ScriptedProvider

    provider = ScriptedProvider()

    def _factory(model):
        return provider

    monkeypatch.setattr("app.providers.get_provider_for_model", _factory)
    return provider


def _make_task(session: Session, user_id: int, **overrides) -> ScheduledTask:
    kwargs = {
        "user_id": user_id,
        "name": "test task",
        "prompt": "Do the thing",
        "cron_expression": "0 9 * * *",
    }
    kwargs.update(overrides)
    return create_task(session, **kwargs)


# --- Cron helpers ---------------------------------------------------------


class TestCronHelpers:
    def test_validate_cron_normalizes(self):
        assert validate_cron("0   9 * * *") == "0 9 * * *"

    @pytest.mark.parametrize("expr", ["", "not a cron", "0 9 * *", "99 9 * * *"])
    def test_validate_cron_rejects_garbage(self, expr):
        with pytest.raises(ValueError):
            validate_cron(expr)
        assert is_valid_cron(expr) is False

    def test_next_cron_run_is_utc_and_in_future(self):
        nxt = next_cron_run("*/5 * * * *")
        assert nxt.tzinfo is not None
        assert nxt > datetime.now(UTC)

    def test_next_cron_run_respects_timezone(self):
        """09:00 in Berlin is 07:00 or 08:00 UTC — never 09:00."""
        nxt = next_cron_run("0 9 * * *", timezone="Europe/Berlin")
        assert nxt.hour in (7, 8)

    def test_next_cron_runs_are_ordered(self):
        runs = next_cron_runs("0 * * * *", count=3)
        assert len(runs) == 3
        assert runs == sorted(runs)

    def test_unknown_timezone_falls_back_to_utc(self):
        nxt = next_cron_run("0 9 * * *", timezone="Mars/Olympus")
        assert nxt.hour == 9

    @pytest.mark.parametrize(
        ("expr", "needle"),
        [
            ("*/15 * * * *", "every 15 minutes"),
            ("0 9 * * *", "daily"),
            ("30 7 * * 1-5", "weekday"),
            ("0 9 * * 1", "Monday"),
            ("0 9 1 * *", "monthly"),
        ],
    )
    def test_describe_cron(self, expr, needle):
        assert needle in describe_cron(expr)

    def test_describe_cron_falls_back_to_expression(self):
        assert describe_cron("nonsense") == "nonsense"


class TestNaturalLanguageSchedules:
    @pytest.mark.parametrize(
        ("text", "expected"),
        [
            ("каждый день в 8 вечера", "0 20 * * *"),
            ("каждый день в 7:30", "30 7 * * *"),
            ("every day at 9am", "0 9 * * *"),
            ("каждые 15 минут", "*/15 * * * *"),
            ("every 30 minutes", "*/30 * * * *"),
            ("каждый час", "0 * * * *"),
            ("hourly", "0 * * * *"),
            ("каждые 6 часов", "0 */6 * * *"),
            ("каждый понедельник в 9 утра", "0 9 * * 1"),
            ("every monday at 9am", "0 9 * * 1"),
            ("по будням в 7:30", "30 7 * * 1-5"),
            ("every weekday at 18:00", "0 18 * * 1-5"),
            ("по выходным в 11:00", "0 11 * * 0,6"),
            ("каждую среду в 18:30", "30 18 * * 3"),
            ("ежемесячно", "0 9 1 * *"),
        ],
    )
    def test_parse_natural_schedule(self, text, expected):
        assert parse_natural_schedule(text) == expected

    def test_parsed_expressions_are_valid_cron(self):
        assert is_valid_cron(parse_natural_schedule("каждый вторник в 18:30"))

    @pytest.mark.parametrize("text", ["", "как-нибудь потом", "when I feel like it"])
    def test_unparseable_returns_none(self, text):
        assert parse_natural_schedule(text) is None


class TestQuietHours:
    def test_inside_simple_window(self):
        moment = datetime(2026, 7, 31, 13, 0, tzinfo=UTC)
        assert in_quiet_hours(moment, timezone="UTC", start="12:00", end="14:00") is True

    def test_outside_simple_window(self):
        moment = datetime(2026, 7, 31, 15, 0, tzinfo=UTC)
        assert in_quiet_hours(moment, timezone="UTC", start="12:00", end="14:00") is False

    def test_window_wrapping_midnight(self):
        late = datetime(2026, 7, 31, 23, 30, tzinfo=UTC)
        early = datetime(2026, 7, 31, 3, 0, tzinfo=UTC)
        noon = datetime(2026, 7, 31, 12, 0, tzinfo=UTC)
        assert in_quiet_hours(late, timezone="UTC", start="23:00", end="07:00") is True
        assert in_quiet_hours(early, timezone="UTC", start="23:00", end="07:00") is True
        assert in_quiet_hours(noon, timezone="UTC", start="23:00", end="07:00") is False

    def test_missing_bounds_mean_no_quiet_hours(self):
        moment = datetime(2026, 7, 31, 3, 0, tzinfo=UTC)
        assert in_quiet_hours(moment, timezone="UTC", start=None, end=None) is False
        assert in_quiet_hours(moment, timezone="UTC", start="oops", end="07:00") is False


# --- Task CRUD ------------------------------------------------------------


class TestTaskCRUD:
    def test_create_computes_next_run(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, name="digest")
            assert task.id is not None
            assert task.enabled is True
            assert task.next_run_at is not None
            assert task.cron_expression == "0 9 * * *"
            assert task.approval_policy == APPROVAL_DENY_EXTERNAL

    def test_create_rejects_invalid_cron(self, user_id):
        with Session(engine) as session, pytest.raises(ValueError):
            _make_task(session, user_id, cron_expression="every wednesday")

    def test_create_requires_cron_for_cron_trigger(self, user_id):
        with Session(engine) as session, pytest.raises(ValueError):
            create_task(session, user_id=user_id, name="x", prompt="p")

    def test_create_rejects_unknown_approval_policy(self, user_id):
        with Session(engine) as session, pytest.raises(ValueError):
            _make_task(session, user_id, approval_policy="whatever")

    def test_disabled_task_has_no_next_run(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, enabled=False)
            assert task.next_run_at is None

    def test_interval_trigger(self, user_id):
        with Session(engine) as session:
            task = create_task(
                session,
                user_id=user_id,
                name="interval",
                prompt="p",
                trigger_type=TRIGGER_INTERVAL,
                interval_seconds=3600,
            )
            assert task.next_run_at is not None
            assert as_utc(task.next_run_at) > datetime.now(UTC)

    def test_interval_trigger_requires_seconds(self, user_id):
        with Session(engine) as session, pytest.raises(ValueError):
            create_task(
                session,
                user_id=user_id,
                name="interval",
                prompt="p",
                trigger_type=TRIGGER_INTERVAL,
            )

    def test_one_shot_date_trigger(self, user_id):
        future = datetime.now(UTC) + timedelta(hours=2)
        with Session(engine) as session:
            task = create_task(
                session,
                user_id=user_id,
                name="reminder",
                prompt="ping me",
                trigger_type=TRIGGER_DATE,
                run_at=future,
            )
            assert task.next_run_at is not None
            # A past one-shot never fires again.
            task.run_at = datetime.now(UTC) - timedelta(hours=1)
            assert compute_next_run(task) is None

    def test_update_reschedules(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            before = task.next_run_at
            updated = update_task(session, task.id, cron_expression="*/5 * * * *")
            assert updated.cron_expression == "*/5 * * * *"
            assert updated.next_run_at != before

    def test_update_rejects_invalid_cron(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            with pytest.raises(ValueError):
                update_task(session, task.id, cron_expression="nope")

    def test_disable_via_update_clears_next_run(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            updated = update_task(session, task.id, enabled=False)
            assert updated.enabled is False
            assert updated.next_run_at is None

    def test_update_missing_task_returns_none(self):
        with Session(engine) as session:
            assert update_task(session, 999999, name="x") is None

    def test_list_filters_by_enabled(self, user_id):
        with Session(engine) as session:
            on = _make_task(session, user_id, name="on")
            off = _make_task(session, user_id, name="off", enabled=False)
            enabled_ids = {t.id for t in list_tasks(session, enabled=True)}
            assert on.id in enabled_ids
            assert off.id not in enabled_ids

    def test_delete_removes_task_and_runs(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            run = create_task_run(session, task)
            task_id, run_id = task.id, run.id
            assert delete_task(session, task_id) is True
            assert get_task(session, task_id) is None
            assert get_task_run(session, run_id) is None
            assert delete_task(session, task_id) is False


# --- Run creation & gating ------------------------------------------------


class TestTaskRunCreation:
    def test_run_gets_isolated_conversation(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, name="iso")
            run = create_task_run(session, task)
            assert run.status == TASK_RUN_QUEUED
            assert run.run_id is not None
            conv = session.get(Conversation, run.conversation_id)
            assert conv is not None
            assert "[Task]" in (conv.title or "")
            assert (conv.metadata_ or {}).get("is_task") is True

    def test_prompt_persisted_in_isolated_conversation(self, user_id):
        from app.agent.service import list_messages

        with Session(engine) as session:
            task = _make_task(session, user_id, prompt="Summarize my inbox")
            run = create_task_run(session, task)
            messages = list_messages(session, run.conversation_id)
            assert messages[0].role == "user"
            assert messages[0].content == "Summarize my inbox"

    def test_task_conversations_hidden_from_chat_list(self, user_id):
        from app.agent.service import list_conversations

        with Session(engine) as session:
            task = _make_task(session, user_id)
            run = create_task_run(session, task)
            visible = {c.id for c in list_conversations(session, user_id=user_id)}
            assert run.conversation_id not in visible

    def test_scheduled_fire_of_disabled_task_is_skipped(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, enabled=False)
            run = prepare_task_run(session, task)
            assert run.status == TASK_RUN_SKIPPED
            assert run.skip_reason == "task disabled"
            assert run.conversation_id is None

    def test_quiet_hours_skip_scheduled_fire(self, user_id):
        now = datetime.now(UTC)
        with Session(engine) as session:
            task = _make_task(
                session,
                user_id,
                quiet_hours_start="00:00",
                quiet_hours_end="23:59",
            )
            run = prepare_task_run(session, task, fire_time=now)
            assert run.status == TASK_RUN_SKIPPED
            assert "quiet hours" in run.skip_reason

    def test_manual_run_ignores_quiet_hours(self, user_id):
        with Session(engine) as session:
            task = _make_task(
                session,
                user_id,
                quiet_hours_start="00:00",
                quiet_hours_end="23:59",
            )
            run = prepare_task_run(
                session,
                task,
                trigger_source=TRIGGER_SOURCE_MANUAL,
                ignore_quiet_hours=True,
            )
            assert run.status == TASK_RUN_QUEUED

    def test_skipped_run_advances_next_run(self, user_id):
        with Session(engine) as session:
            task = _make_task(
                session, user_id, quiet_hours_start="00:00", quiet_hours_end="23:59"
            )
            prepare_task_run(session, task, fire_time=datetime.now(UTC))
            session.refresh(task)
            assert task.last_status == TASK_RUN_SKIPPED
            assert task.next_run_at is not None


# --- Execution ------------------------------------------------------------


class TestTaskExecution:
    async def test_run_completes_and_records_output(self, user_id, scripted):
        scripted.set_script(["Here is your digest."])
        with Session(engine) as session:
            task = _make_task(session, user_id, name="exec")
            run = create_task_run(session, task)
            run_id, task_id = run.id, task.id

        await execute_task_run(run_id)

        with Session(engine) as session:
            run = get_task_run(session, run_id)
            task = get_task(session, task_id)
            assert run.status == TASK_RUN_COMPLETED
            assert run.output and "digest" in run.output
            assert run.finished_at is not None
            assert run.duration_ms is not None
            assert task.last_status == TASK_RUN_COMPLETED
            assert task.run_count == 1
            assert task.failure_count == 0
            assert task.next_run_at is not None

    async def test_completed_run_is_delivered_to_ui_and_unread(self, user_id, scripted):
        scripted.set_script(["Delivered result."])
        with Session(engine) as session:
            task = _make_task(session, user_id, name="delivery")
            run_id = create_task_run(session, task).id

        await execute_task_run(run_id)

        with Session(engine) as session:
            run = get_task_run(session, run_id)
            assert run.delivery_status == {"ui": "ok"}
            assert run.delivered_at is not None
            assert run.is_read is False

    async def test_identical_output_is_not_redelivered(self, user_id, scripted):
        """An unchanged result is delivered to the UI but deduped for other channels."""
        scripted.set_script(["Same output.", "Same output."])
        with Session(engine) as session:
            task = _make_task(
                session,
                user_id,
                name="dedup",
                delivery_channels=["ui", "webhook"],
                delivery_config={"webhook_url": "https://example.invalid/hook"},
            )
            task_id = task.id
            first = create_task_run(session, task).id

        await execute_task_run(first)

        with Session(engine) as session:
            task = get_task(session, task_id)
            assert task.last_delivery_hash is not None
            second = create_task_run(session, task).id

        await execute_task_run(second)

        with Session(engine) as session:
            run = get_task_run(session, second)
            assert "duplicate" in run.delivery_status["webhook"]

    async def test_provider_failure_marks_run_failed(self, user_id, scripted):
        # Empty script → the provider raises, the loop emits an error event.
        scripted.set_script([])
        with Session(engine) as session:
            task = _make_task(session, user_id, name="failing")
            run_id, task_id = create_task_run(session, task).id, task.id

        await execute_task_run(run_id)

        with Session(engine) as session:
            run = get_task_run(session, run_id)
            task = get_task(session, task_id)
            assert run.status == TASK_RUN_FAILED
            assert run.error
            assert task.failure_count == 1
            assert task.last_status == TASK_RUN_FAILED

    async def test_auto_disable_after_repeated_failures(self, user_id, scripted, monkeypatch):
        from app.core.config import get_settings

        monkeypatch.setattr(get_settings(), "scheduler_max_consecutive_failures", 1)
        scripted.set_script([])
        with Session(engine) as session:
            task = _make_task(session, user_id, name="flaky")
            run_id, task_id = create_task_run(session, task).id, task.id

        await execute_task_run(run_id)

        with Session(engine) as session:
            task = get_task(session, task_id)
            assert task.enabled is False
            assert task.next_run_at is None

    async def test_timeout_fails_the_run(self, user_id, monkeypatch):
        """A run that outlives the task's timeout is failed, not left hanging."""
        import asyncio

        from app.providers import ChatStreamEvent, LLMProvider

        class SlowProvider(LLMProvider):
            name = "slow"

            async def chat_completion(self, messages, *, model, tools=None, **kwargs):
                raise NotImplementedError

            async def chat_completion_stream(self, messages, *, model, **kwargs):
                await asyncio.sleep(5)
                yield ChatStreamEvent(delta="too late", finish=True)

        monkeypatch.setattr("app.providers.get_provider_for_model", lambda model: SlowProvider())
        with Session(engine) as session:
            task = _make_task(session, user_id, name="slowpoke", timeout_s=0.05)
            run_id = create_task_run(session, task).id

        await execute_task_run(run_id)

        with Session(engine) as session:
            run = get_task_run(session, run_id)
            assert run.status == TASK_RUN_FAILED
            assert "timed out" in run.error

    async def test_missing_task_row_fails_run_gracefully(self, user_id, scripted):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            run_id = create_task_run(session, task).id
            session.delete(session.get(ScheduledTask, task.id))
            session.commit()

        assert await execute_task_run(run_id) is None
        with Session(engine) as session:
            assert get_task_run(session, run_id).status == TASK_RUN_FAILED

    async def test_run_task_now_awaits_result(self, user_id, scripted):
        scripted.set_script(["Immediate answer."])
        with Session(engine) as session:
            task = _make_task(session, user_id, name="manual")
            run = await run_task_now(session, task.id)
            assert run.status == TASK_RUN_COMPLETED
            assert run.trigger_source == TRIGGER_SOURCE_MANUAL
            assert "Immediate" in run.output

    async def test_run_task_now_unknown_task(self):
        with Session(engine) as session:
            assert await run_task_now(session, 999999) is None


class TestApprovalPolicy:
    def test_deny_external_by_default(self, user_id):
        from app.tasks.service import _resolve_approval

        with Session(engine) as session:
            task = _make_task(session, user_id)
            policy, reason = _resolve_approval(task)
            assert policy["send_external"] == "deny"
            assert "external" in reason

    def test_allow_all_leaves_policy_untouched(self, user_id):
        from app.tasks.service import _resolve_approval

        with Session(engine) as session:
            task = _make_task(session, user_id, approval_policy=APPROVAL_ALLOW_ALL)
            policy, reason = _resolve_approval(task)
            assert "send_external" not in policy
            assert "Pre-approved" in reason

    def test_explicit_capability_policy_wins(self, user_id):
        from app.tasks.service import _resolve_approval

        with Session(engine) as session:
            task = _make_task(
                session, user_id, capability_policy={"send_external": "allow", "write": "deny"}
            )
            policy, _ = _resolve_approval(task)
            assert policy["send_external"] == "allow"
            assert policy["write"] == "deny"

    async def test_decision_recorded_on_the_run(self, user_id, scripted):
        scripted.set_script(["ok"])
        with Session(engine) as session:
            task = _make_task(session, user_id, name="audited")
            run_id = create_task_run(session, task).id

        await execute_task_run(run_id)

        with Session(engine) as session:
            run = get_task_run(session, run_id)
            assert run.approval_policy == APPROVAL_DENY_EXTERNAL
            assert run.approval_reason


# --- Cancellation & inbox -------------------------------------------------


class TestCancellation:
    def test_cancel_queued_run(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            run = create_task_run(session, task)
            assert cancel_task_run(session, run.id) is True
            session.refresh(run)
            assert run.status == "cancelled"

    def test_cancel_terminal_run_returns_false(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id)
            run = create_task_run(session, task)
            run.status = TASK_RUN_COMPLETED
            session.add(run)
            session.commit()
            assert cancel_task_run(session, run.id) is False

    def test_cancel_unknown_run(self):
        with Session(engine) as session:
            assert cancel_task_run(session, 999999) is False


class TestInbox:
    def test_mark_read_and_unread_count(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, name="inbox")
            run = create_task_run(session, task)
            assert run.is_read is False
            before = unread_count(session)
            updated = mark_run_read(session, run.id)
            assert updated.is_read is True
            assert unread_count(session) == before - 1

    def test_unread_filter(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, name="unread-filter")
            run = create_task_run(session, task)
            unread_ids = {r.id for r in list_task_runs(session, unread_only=True)}
            assert run.id in unread_ids
            mark_run_read(session, run.id)
            unread_ids = {r.id for r in list_task_runs(session, unread_only=True)}
            assert run.id not in unread_ids

    def test_mark_read_unknown_run(self):
        with Session(engine) as session:
            assert mark_run_read(session, 999999) is None

    def test_runs_listed_per_task_newest_first(self, user_id):
        with Session(engine) as session:
            task = _make_task(session, user_id, name="history")
            first = create_task_run(session, task)
            second = create_task_run(session, task)
            runs = list_task_runs(session, task_id=task.id)
            assert [r.id for r in runs] == [second.id, first.id]


# --- Templates ------------------------------------------------------------


class TestTemplates:
    def test_builtin_templates_present(self):
        from app.tasks.templates import list_templates

        slugs = {t["slug"] for t in list_templates()}
        assert {"news-digest", "code-review", "memory-review", "health-check"} <= slugs

    def test_template_crons_are_valid(self):
        from app.tasks.templates import TASK_TEMPLATES

        for template in TASK_TEMPLATES:
            assert is_valid_cron(template.cron_expression), template.slug

    def test_template_tools_are_registered(self):
        from app.tasks.templates import TASK_TEMPLATES
        from app.tools import get_registry

        registered = set(get_registry())
        for template in TASK_TEMPLATES:
            for name in template.tools_whitelist or []:
                assert name in registered, f"{template.slug}: unknown tool {name}"

    def test_get_unknown_template(self):
        from app.tasks.templates import get_template

        assert get_template("nope") is None


# --- Scheduler engine -----------------------------------------------------


class TestSchedulerEngine:
    def test_build_trigger_per_type(self, user_id):
        from apscheduler.triggers.cron import CronTrigger
        from apscheduler.triggers.date import DateTrigger
        from apscheduler.triggers.interval import IntervalTrigger

        from app.tasks.scheduler import build_trigger

        with Session(engine) as session:
            cron_task = _make_task(session, user_id, name="cron")
            interval_task = create_task(
                session,
                user_id=user_id,
                name="interval",
                prompt="p",
                trigger_type=TRIGGER_INTERVAL,
                interval_seconds=60,
            )
            date_task = create_task(
                session,
                user_id=user_id,
                name="date",
                prompt="p",
                trigger_type=TRIGGER_DATE,
                run_at=datetime.now(UTC) + timedelta(hours=1),
            )
            assert isinstance(build_trigger(cron_task), CronTrigger)
            assert isinstance(build_trigger(interval_task), IntervalTrigger)
            assert isinstance(build_trigger(date_task), DateTrigger)

    def test_sync_is_noop_without_running_engine(self, user_id):
        from app.tasks.scheduler import remove_task_job, sync_task_job

        with Session(engine) as session:
            task = _make_task(session, user_id)
            sync_task_job(task)  # must not raise
            remove_task_job(task.id)

    def test_status_reports_disabled_in_tests(self):
        from app.tasks.scheduler import scheduler_status

        status = scheduler_status()
        assert status["enabled"] is False
        assert status["running"] is False
        assert status["jobs"] == []

    async def test_start_registers_jobs_and_shutdown_stops(self, user_id, monkeypatch):
        from app.core.config import get_settings
        from app.tasks.scheduler import (
            is_running,
            job_id,
            scheduler_status,
            shutdown_scheduler,
            start_scheduler,
        )

        monkeypatch.setattr(get_settings(), "scheduler_enabled", True)
        with Session(engine) as session:
            task = _make_task(session, user_id, name="engine", cron_expression="0 3 * * *")
            task_id = task.id
        try:
            count = await start_scheduler()
            assert count >= 1
            assert is_running() is True
            job_ids = {j["id"] for j in scheduler_status()["jobs"]}
            assert job_id(task_id) in job_ids
        finally:
            await shutdown_scheduler()
        assert is_running() is False

    def test_catch_up_skips_missed_fire(self, user_id):
        from app.tasks.scheduler import _catch_up

        with Session(engine) as session:
            task = _make_task(session, user_id, name="missed")
            task.next_run_at = datetime.now(UTC) - timedelta(days=1)
            session.add(task)
            session.commit()

            _catch_up(session, task, now=datetime.now(UTC))
            session.refresh(task)
            assert task.last_status == TASK_RUN_SKIPPED
            assert as_utc(task.next_run_at) > datetime.now(UTC)

    def test_catch_up_leaves_future_fire_alone(self, user_id):
        from app.tasks.scheduler import _catch_up

        with Session(engine) as session:
            task = _make_task(session, user_id, name="future", misfire_policy=MISFIRE_RUN)
            before = task.next_run_at
            _catch_up(session, task, now=datetime.now(UTC))
            session.refresh(task)
            assert task.next_run_at == before
            assert task.last_status is None


# --- Agent-facing tools ---------------------------------------------------


class TestScheduleTools:
    async def test_parse_cron_tool(self):
        from app.tools import get_tool

        result = await get_tool("parse_cron").run({"text": "каждый день в 8 вечера"})
        assert result.is_error is False
        assert "0 20 * * *" in result.output

    async def test_parse_cron_tool_rejects_gibberish(self):
        from app.tools import get_tool

        result = await get_tool("parse_cron").run({"text": "as soon as possible"})
        assert result.is_error is True

    async def test_create_and_list_task_tool(self):
        from app.tools import get_tool

        created = await get_tool("create_task").run(
            {
                "name": "tool digest",
                "prompt": "Send me a digest",
                "schedule": "каждый понедельник в 9 утра",
            }
        )
        assert created.is_error is False
        task_id = created.metadata["task_id"]

        with Session(engine) as session:
            task = get_task(session, task_id)
            assert task.cron_expression == "0 9 * * 1"
            assert task.enabled is True

        listed = await get_tool("list_tasks").run({})
        assert listed.is_error is False
        assert "tool digest" in listed.output

    async def test_create_task_tool_from_template(self):
        from app.tools import get_tool

        created = await get_tool("create_task").run(
            {"name": "templated", "template": "news-digest"}
        )
        assert created.is_error is False
        with Session(engine) as session:
            task = get_task(session, created.metadata["task_id"])
            assert task.workflow_type == "news-digest"
            assert task.cron_expression == "0 8 * * *"
            assert task.prompt

    async def test_create_task_tool_rejects_unknown_template(self):
        from app.tools import get_tool

        result = await get_tool("create_task").run({"name": "x", "template": "nope"})
        assert result.is_error is True

    async def test_create_task_tool_requires_schedule(self):
        from app.tools import get_tool

        result = await get_tool("create_task").run({"name": "x", "prompt": "p"})
        assert result.is_error is True
        assert "schedule" in result.output

    async def test_create_task_tool_rejects_unparseable_schedule(self):
        from app.tools import get_tool

        result = await get_tool("create_task").run(
            {"name": "x", "prompt": "p", "schedule": "someday soon"}
        )
        assert result.is_error is True

    async def test_update_and_delete_task_tools(self, user_id):
        from app.tools import get_tool

        with Session(engine) as session:
            task = _make_task(session, user_id, name="tool-update")
            task_id = task.id

        updated = await get_tool("update_task").run(
            {"task_id": task_id, "schedule": "every 30 minutes", "enabled": False}
        )
        assert updated.is_error is False
        with Session(engine) as session:
            task = get_task(session, task_id)
            assert task.cron_expression == "*/30 * * * *"
            assert task.enabled is False

        deleted = await get_tool("delete_task").run({"task_id": task_id})
        assert deleted.is_error is False
        with Session(engine) as session:
            assert get_task(session, task_id) is None

    async def test_update_task_tool_requires_a_field(self, user_id):
        from app.tools import get_tool

        with Session(engine) as session:
            task_id = _make_task(session, user_id, name="tool-noop").id
        result = await get_tool("update_task").run({"task_id": task_id})
        assert result.is_error is True

    async def test_run_task_now_tool(self, user_id, scripted):
        from app.tools import get_tool

        scripted.set_script(["Tool-driven output."])
        with Session(engine) as session:
            task_id = _make_task(session, user_id, name="tool-run").id

        result = await get_tool("run_task_now").run({"task_id": task_id})
        assert result.is_error is False
        assert "Tool-driven output" in result.output

    async def test_run_task_now_tool_unknown_task(self):
        from app.tools import get_tool

        result = await get_tool("run_task_now").run({"task_id": 999999})
        assert result.is_error is True


# --- REST API -------------------------------------------------------------


def _client() -> TestClient:
    from app.main import app

    return TestClient(app)


class TestTasksApi:
    def test_crud_roundtrip(self):
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={
                    "name": "api task",
                    "prompt": "Do API things",
                    "cron_expression": "0 7 * * *",
                },
            )
            assert resp.status_code == 201, resp.text
            task = resp.json()
            task_id = task["id"]
            assert task["schedule_description"]
            assert len(task["next_runs"]) == 3

            resp = c.get("/api/tasks")
            assert resp.status_code == 200
            assert any(t["id"] == task_id for t in resp.json())

            resp = c.get(f"/api/tasks/{task_id}")
            assert resp.status_code == 200
            assert resp.json()["prompt"] == "Do API things"

            resp = c.put(f"/api/tasks/{task_id}", json={"enabled": False})
            assert resp.status_code == 200
            assert resp.json()["enabled"] is False
            assert resp.json()["next_run_at"] is None

            resp = c.delete(f"/api/tasks/{task_id}")
            assert resp.status_code == 204
            assert c.get(f"/api/tasks/{task_id}").status_code == 404

    def test_create_rejects_invalid_cron(self):
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={"name": "bad", "prompt": "p", "cron_expression": "nope"},
            )
            assert resp.status_code == 422

    def test_create_requires_prompt(self):
        with _client() as c:
            resp = c.post("/api/tasks", json={"name": "bad", "cron_expression": "0 9 * * *"})
            assert resp.status_code == 422

    def test_create_from_template(self):
        with _client() as c:
            resp = c.post("/api/tasks", json={"name": "tpl", "template": "memory-review"})
            assert resp.status_code == 201, resp.text
            body = resp.json()
            assert body["workflow_type"] == "memory-review"
            assert body["cron_expression"] == "0 9 * * 1"

    def test_create_from_unknown_template(self):
        with _client() as c:
            resp = c.post("/api/tasks", json={"name": "tpl", "template": "ghost"})
            assert resp.status_code == 404

    def test_update_without_fields_is_rejected(self):
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={"name": "u", "prompt": "p", "cron_expression": "0 9 * * *"},
            )
            task_id = resp.json()["id"]
            assert c.put(f"/api/tasks/{task_id}", json={}).status_code == 422

    def test_missing_task_endpoints_404(self):
        with _client() as c:
            assert c.get("/api/tasks/999999").status_code == 404
            assert c.put("/api/tasks/999999", json={"name": "x"}).status_code == 404
            assert c.delete("/api/tasks/999999").status_code == 404
            assert c.post("/api/tasks/999999/run").status_code == 404
            assert c.get("/api/tasks/999999/runs").status_code == 404

    def test_parse_cron_endpoint(self):
        with _client() as c:
            resp = c.post("/api/tasks/parse-cron", json={"text": "every weekday at 7:30"})
            assert resp.status_code == 200
            body = resp.json()
            assert body["cron_expression"] == "30 7 * * 1-5"
            assert len(body["next_runs"]) == 3

            resp = c.post("/api/tasks/parse-cron", json={"text": "0 9 * * *"})
            assert resp.json()["cron_expression"] == "0 9 * * *"

            resp = c.post("/api/tasks/parse-cron", json={"text": "no idea"})
            assert resp.json()["cron_expression"] is None
            assert resp.json()["detail"]

    def test_templates_and_scheduler_endpoints(self):
        with _client() as c:
            resp = c.get("/api/tasks/templates")
            assert resp.status_code == 200
            assert any(t["slug"] == "news-digest" for t in resp.json())

            resp = c.get("/api/tasks/scheduler")
            assert resp.status_code == 200
            assert resp.json()["enabled"] is False

    def test_manual_trigger_and_run_history(self, scripted):
        scripted.set_script(["API-triggered output."])
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={"name": "trigger", "prompt": "p", "cron_expression": "0 9 * * *"},
            )
            task_id = resp.json()["id"]

            resp = c.post(f"/api/tasks/{task_id}/run")
            assert resp.status_code == 202, resp.text
            run = resp.json()
            assert run["trigger_source"] == "manual"

            resp = c.get(f"/api/tasks/{task_id}/runs")
            assert resp.status_code == 200
            assert any(r["id"] == run["id"] for r in resp.json())

            resp = c.get(f"/api/tasks/runs/{run['id']}")
            assert resp.status_code == 200
            assert resp.json()["messages"]

    def test_manual_trigger_executes_in_background(self, scripted):
        """The /run endpoint must actually spawn execution, not leave it queued.

        Regression: a sync endpoint runs in a threadpool with no event loop, so
        the background spawn silently no-ops and the run stays 'queued' forever.
        """
        import time

        scripted.set_script(["Background done."])
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={"name": "bg exec", "prompt": "p", "cron_expression": "0 9 * * *"},
            )
            task_id = resp.json()["id"]
            resp = c.post(f"/api/tasks/{task_id}/run")
            assert resp.status_code == 202, resp.text
            run_id = resp.json()["id"]

            run: dict = {}
            status = "queued"
            for _ in range(50):  # up to ~5s
                time.sleep(0.1)
                run = c.get(f"/api/tasks/runs/{run_id}").json()
                status = run["status"]
                if status in ("completed", "failed", "cancelled"):
                    break
            assert status == "completed", f"run stuck at {status!r}"
            assert "Background done" in (run.get("output") or "")

    def test_inbox_and_read_state(self):
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={"name": "inbox api", "prompt": "p", "cron_expression": "0 9 * * *"},
            )
            task_id = resp.json()["id"]
            with Session(engine) as session:
                run_id = create_task_run(session, get_task(session, task_id)).id

            resp = c.get("/api/tasks/inbox?unread_only=true")
            assert resp.status_code == 200
            body = resp.json()
            assert body["unread_count"] >= 1
            assert any(r["id"] == run_id for r in body["runs"])

            resp = c.post(f"/api/tasks/runs/{run_id}/read", json={"is_read": True})
            assert resp.status_code == 200
            assert resp.json()["is_read"] is True

    def test_run_endpoints_404_for_unknown_run(self):
        with _client() as c:
            assert c.get("/api/tasks/runs/999999").status_code == 404
            assert (
                c.post("/api/tasks/runs/999999/read", json={"is_read": True}).status_code == 404
            )
            assert c.post("/api/tasks/runs/999999/cancel").status_code == 404

    def test_cancel_run_endpoint(self):
        with _client() as c:
            resp = c.post(
                "/api/tasks",
                json={"name": "cancel api", "prompt": "p", "cron_expression": "0 9 * * *"},
            )
            task_id = resp.json()["id"]
            with Session(engine) as session:
                run_id = create_task_run(session, get_task(session, task_id)).id

            resp = c.post(f"/api/tasks/runs/{run_id}/cancel")
            assert resp.status_code == 200
            assert resp.json()["cancelled"] is True
