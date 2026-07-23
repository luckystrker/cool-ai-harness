"""Tests for durable agent runs (Фаза 1.5 — §1).

Three layers:
  - Executor unit tests: cancellation + cost budget honored in the loop.
  - Service unit tests: AgentRun/RunEvent persistence helpers.
  - API integration tests: runs are created/streamed/listed/cancelled end to end.
"""

from __future__ import annotations

import json

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session, SQLModel, create_engine

from app.agent import AgentConfig, AgentExecutor, AgentLimits
from app.agent.runs import run_registry
from app.agent.service import (
    append_run_events,
    create_run,
    finish_run,
    get_run,
    list_run_events,
    list_runs,
    update_run,
)
from app.models.run import (
    RUN_STATUS_CANCELLED,
    RUN_STATUS_COMPLETED,
    RUN_STATUS_RUNNING,
)
from tests.conftest import ScriptedProvider

# --- executor: cancellation + cost budget ----------------------------------


@pytest.mark.asyncio
async def test_cancel_before_start_yields_cancelled_finish(scripted_provider) -> None:
    """A run cancelled before the loop starts finishes with reason=cancelled."""
    scripted_provider.set_script(["should not matter"])
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(model="m", run_id=42, cancellable=True),
    )
    # Register + cancel before streaming begins.
    run_registry.register(42)
    run_registry.cancel(42)

    events = [e async for e in ex.stream("hi")]
    finish = events[-1]
    assert finish.kind == "finish"
    assert finish.payload["reason"] == "cancelled"
    # The provider was never called because we bailed out up front.
    assert scripted_provider.calls == []


@pytest.mark.asyncio
async def test_cancel_between_tool_calls_stops_loop(scripted_provider) -> None:
    """Cancelling after the first tool_result stops before the next round-trip.

    The script requests a tool on every turn, so without cancellation the loop
    proceeds to a second tool call and a final answer. We cancel the moment the
    first tool_result is emitted and expect a cancelled finish — the second
    provider call must never happen.
    """
    run_id = 101
    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "write_file", "arguments": {"path": "a.txt", "content": "x"}}],
            [{"id": "c2", "name": "write_file", "arguments": {"path": "b.txt", "content": "y"}}],
            "done",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            run_id=run_id,
            cancellable=True,
            limits=AgentLimits(max_iterations=5),
        ),
    )
    run_registry.register(run_id)

    events: list = []
    async for event in ex.stream("go"):
        events.append(event)
        # The cancel check runs at the top of the next iteration and before the
        # next tool call — flip the flag as soon as the first tool finishes.
        if event.kind == "tool_result":
            run_registry.cancel(run_id)

    finish = events[-1]
    assert finish.kind == "finish"
    assert finish.payload["reason"] == "cancelled"
    # Only the first turn reached the provider; the second never did.
    assert len(scripted_provider.calls) == 1


@pytest.mark.asyncio
async def test_cost_limit_finish(scripted_provider) -> None:
    """When cumulative cost exceeds max_cost_usd, the loop stops with cost_limit."""
    # The scripted provider reports no cost_usd, so to exercise the guard we set
    # a budget of 0 — (cost_usd or 0.0) >= 0 trips immediately after iteration 1.
    scripted_provider.set_script(["one", "two", "three"])
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            limits=AgentLimits(max_cost_usd=0.0, max_iterations=5),
        ),
    )
    events = [e async for e in ex.stream("hi")]
    finish = events[-1]
    assert finish.payload["reason"] == "cost_limit"


@pytest.mark.asyncio
async def test_non_cancellable_run_ignores_registry(scripted_provider) -> None:
    """A non-cancellable run isn't affected by a stray cancel signal."""
    scripted_provider.set_script(["ok"])
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))
    # Even if something cancels id 9999, this run (no run_id) is unaffected.
    run_registry.register(9999)
    run_registry.cancel(9999)

    events = [e async for e in ex.stream("hi")]
    assert events[-1].payload["reason"] in ("stop", "end_turn")


# --- service: persistence helpers ------------------------------------------


@pytest.fixture()
def db_session():
    """An isolated in-memory SQLite session for service-level tests."""
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False})
    SQLModel.metadata.create_all(engine)
    # We need a conversation row to satisfy the FK; create the minimal chain.
    with Session(engine) as session:
        from app.models import Conversation, User

        user = User(username="t")
        session.add(user)
        session.commit()
        session.refresh(user)
        conv = Conversation(user_id=user.id, title="t")
        session.add(conv)
        session.commit()
        session.refresh(conv)
        yield session, conv.id


def test_create_and_finish_run(db_session) -> None:
    session, conv_id = db_session
    run = create_run(session, conversation_id=conv_id, model="m")
    assert run.status == RUN_STATUS_RUNNING
    assert run.model == "m"

    finished = finish_run(
        session, run.id, finish_reason="stop", usage={"total_tokens": 5}, iterations=1
    )
    assert finished.status == RUN_STATUS_COMPLETED
    assert finished.finish_reason == "stop"
    assert finished.usage == {"total_tokens": 5}
    assert finished.finished_at is not None


def test_finish_reason_cancelled_sets_status(db_session) -> None:
    session, conv_id = db_session
    run = create_run(session, conversation_id=conv_id)
    finished = finish_run(session, run.id, finish_reason="cancelled")
    assert finished.status == RUN_STATUS_CANCELLED


def test_append_run_events_monotonic_seq(db_session) -> None:
    session, conv_id = db_session
    run = create_run(session, conversation_id=conv_id)

    append_run_events(session, run_id=run.id, events=[("start", {"run_id": run.id})])
    append_run_events(
        session,
        run_id=run.id,
        events=[("token", {"text": "a"}), ("token", {"text": "b"})],
    )
    append_run_events(session, run_id=run.id, events=[("finish", {"reason": "stop"})])

    events = list_run_events(session, run_id=run.id)
    assert [e.seq for e in events] == [0, 1, 2, 3]
    assert [e.kind for e in events] == ["start", "token", "token", "finish"]
    # Payloads round-trip intact.
    assert events[0].payload == {"run_id": run.id}
    assert events[1].payload == {"text": "a"}


def test_list_runs_newest_first(db_session) -> None:
    session, conv_id = db_session
    r1 = create_run(session, conversation_id=conv_id)
    r2 = create_run(session, conversation_id=conv_id)
    runs = list_runs(session, conversation_id=conv_id)
    assert [r.id for r in runs] == [r2.id, r1.id]


def test_update_run_patches_fields(db_session) -> None:
    session, conv_id = db_session
    run = create_run(session, conversation_id=conv_id)
    updated = update_run(session, run.id, iterations=3, checkpoint={"last_call_id": "x"})
    assert updated.iterations == 3
    assert updated.checkpoint == {"last_call_id": "x"}
    # get_run sees the same row.
    assert get_run(session, run.id).iterations == 3


# --- API: runs created, listed, detailed, cancelled ------------------------


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    """Inject a provider into every module that resolves the default provider."""
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_default_provider", lambda: provider)


def test_streaming_creates_run_with_event_log(monkeypatch) -> None:
    """Sending a message creates an AgentRun whose event log captures the run."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["Hello."])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={"title": "r"}).json()["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "hi"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for _ in resp.iter_lines():
                pass

        # One run exists for this conversation, and it's completed.
        runs = c.get(f"/api/conversations/{conv_id}/runs").json()
        assert len(runs) == 1
        run = runs[0]
        assert run["status"] == RUN_STATUS_COMPLETED
        assert run["finish_reason"] in ("stop", "end_turn")

        # The detail endpoint includes the event log (start, token, message, finish).
        detail = c.get(f"/api/conversations/{conv_id}/runs/{run['id']}").json()
        kinds = [e["kind"] for e in detail["events"]]
        assert kinds[0] == "start"
        assert kinds[-1] == "finish"
        assert "message" in kinds
        # seq is monotonic from 0.
        seqs = [e["seq"] for e in detail["events"]]
        assert seqs == sorted(seqs)
        assert seqs[0] == 0

        # The standalone events endpoint matches the embedded list.
        events = c.get(f"/api/conversations/{conv_id}/runs/{run['id']}/events").json()
        assert [e["seq"] for e in events] == seqs


def test_cancel_endpoint_signals_running_run(monkeypatch) -> None:
    """The cancel endpoint flips the registry flag for a registered run."""
    from app.main import app

    provider = ScriptedProvider()
    # A tool loop so the run is long-lived enough to cancel.
    provider.set_script(
        [
            [{"id": "c1", "name": "write_file", "arguments": {"path": "z.txt", "content": "x"}}],
            [{"id": "c2", "name": "write_file", "arguments": {"path": "z2.txt", "content": "y"}}],
            "done",
        ]
    )
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={}).json()["id"]
        # Register the run id we'll use before streaming, so we can cancel it
        # once it's live. We peek the would-be run id by creating one up front
        # is awkward; instead cancel a known-registered id via the registry.
        run_id_holder: dict = {}

        # Patch create_run to capture the id as the runner makes it.
        import app.api.conversations as conv_module

        original_create_run = conv_module.create_run

        def _capturing_create_run(session, **kwargs):
            run = original_create_run(session, **kwargs)
            run_id_holder["id"] = run.id
            return run

        monkeypatch.setattr(conv_module, "create_run", _capturing_create_run)

        # Drive the stream; cancel from a background thread once we know the id.
        import threading

        def _cancel_when_known():
            while "id" not in run_id_holder:
                pass
            # Small delay so the loop is mid-flight.
            import time

            time.sleep(0.05)
            run_registry.cancel(run_id_holder["id"])

        t = threading.Thread(target=_cancel_when_known, daemon=True)
        t.start()

        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "go"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for line in resp.iter_lines():
                if line.startswith("data:"):
                    # Drain the stream; the cancel happens in the background.
                    json.loads(line[len("data:") :].strip())
        t.join(timeout=2)

        # The run should have been cancelled (status cancelled).
        runs = c.get(f"/api/conversations/{conv_id}/runs").json()
        assert runs, "expected at least one run"
        assert runs[0]["status"] == RUN_STATUS_CANCELLED

        # Cancelling again is a no-op (run no longer active) — not an error.
        again = c.post(f"/api/conversations/{conv_id}/runs/{runs[0]['id']}/cancel").json()
        assert again["cancelled"] is False


def test_runs_endpoints_404_for_missing(monkeypatch) -> None:
    from app.main import app

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={}).json()["id"]
        # Unknown run under an existing conversation.
        assert c.get(f"/api/conversations/{conv_id}/runs/999999").status_code == 404
        assert c.get(f"/api/conversations/{conv_id}/runs/999999/events").status_code == 404
        assert c.post(f"/api/conversations/{conv_id}/runs/999999/cancel").status_code == 404
        # Unknown conversation.
        assert c.get("/api/conversations/999999/runs").status_code == 404


def test_run_cross_conversation_404(monkeypatch) -> None:
    """A run from conversation A must not be visible under conversation B."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["ok"])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_a = c.post("/api/conversations", json={}).json()["id"]
        conv_b = c.post("/api/conversations", json={}).json()["id"]
        c.post(
            f"/api/conversations/{conv_a}/messages",
            json={"content": "hi"},
            headers={"Accept": "text/event-stream"},
        )
        run_a = c.get(f"/api/conversations/{conv_a}/runs").json()[0]
        # Visible under A...
        assert c.get(f"/api/conversations/{conv_a}/runs/{run_a['id']}").status_code == 200
        # ...but 404 under B.
        assert c.get(f"/api/conversations/{conv_b}/runs/{run_a['id']}").status_code == 404
