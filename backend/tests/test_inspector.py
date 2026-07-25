"""Tests for the Inspector / Debug mode (Фаза 1.5 §6).

Covers:
  - llm_call_complete event emission from the executor
  - Timeline reconstruction from the event log
  - Run comparison (metric deltas)
  - Replay endpoint (creates a new run)
  - API integration (timeline, compare endpoints)
  - InspectorRegistry pub/sub
"""

from __future__ import annotations

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session, SQLModel, create_engine

from app.agent import AgentConfig, AgentExecutor, AgentLimits
from app.agent.service import (
    append_run_events,
    create_run,
    finish_run,
)
from app.observability import InspectorRegistry
from app.observability.inspector import build_run_timeline, compare_runs, prepare_replay
from tests.conftest import ScriptedProvider

# --- executor: llm_call_complete event emission ----------------------------


@pytest.mark.asyncio
async def test_llm_call_complete_event_emitted(scripted_provider) -> None:
    """The executor emits llm_call_complete after each LLM round-trip."""
    scripted_provider.set_script(["Hello world."])
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(model="test-model"),
    )
    events = [e async for e in ex.stream("hi")]
    llm_events = [e for e in events if e.kind == "llm_call_complete"]
    assert len(llm_events) == 1
    payload = llm_events[0].payload
    assert payload["iteration"] == 1
    assert payload["model"] == "test-model"
    assert payload["duration_ms"] >= 0
    assert payload["usage"] is not None
    assert payload["usage"]["total_tokens"] == 15


@pytest.mark.asyncio
async def test_llm_call_complete_multiple_iterations(scripted_provider) -> None:
    """Multiple iterations produce one llm_call_complete each."""
    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "write_file", "arguments": {"path": "a.txt", "content": "x"}}],
            "done",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(model="m", limits=AgentLimits(max_iterations=5)),
    )
    events = [e async for e in ex.stream("go")]
    llm_events = [e for e in events if e.kind == "llm_call_complete"]
    assert len(llm_events) == 2
    assert llm_events[0].payload["iteration"] == 1
    assert llm_events[1].payload["iteration"] == 2


# --- service: timeline reconstruction --------------------------------------


@pytest.fixture()
def db_session():
    """An isolated in-memory SQLite session for service-level tests."""
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False})
    SQLModel.metadata.create_all(engine)
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


def test_build_run_timeline(db_session) -> None:
    """Timeline reconstruction groups events into iterations."""
    session, conv_id = db_session
    run = create_run(session, conversation_id=conv_id, model="m")

    # Seed the event log with a realistic sequence.
    append_run_events(session, run_id=run.id, events=[
        ("start", {"run_id": run.id}),
        ("llm_call_complete", {"iteration": 1, "model": "m", "usage": {"total_tokens": 15}, "duration_ms": 100}),
        ("message", {"content": "thinking...", "tool_calls": [{"id": "c1", "name": "read_file"}]}),
        ("tool_call_start", {"id": "c1", "name": "read_file", "arguments": {"path": "x.txt"}}),
        ("tool_result", {"id": "c1", "name": "read_file", "result": {"output": "data"}}),
        ("llm_call_complete", {"iteration": 2, "model": "m", "usage": {"total_tokens": 20}, "duration_ms": 150}),
        ("message", {"content": "Final answer.", "tool_calls": None}),
        ("finish", {"reason": "stop", "usage": {"total_tokens": 35}, "iterations": 2, "elapsed_ms": 250}),
    ])

    timeline = build_run_timeline(session, run.id)
    assert timeline is not None
    assert len(timeline.iterations) == 2
    assert timeline.iterations[0].iteration == 1
    assert timeline.iterations[0].duration_ms == 100
    assert timeline.iterations[0].usage == {"total_tokens": 15}
    assert len(timeline.iterations[0].tool_calls) == 1
    assert timeline.iterations[0].tool_calls[0]["name"] == "read_file"
    assert timeline.iterations[1].iteration == 2
    assert timeline.iterations[1].duration_ms == 150
    assert timeline.iterations[1].finish_reason == "stop"
    assert timeline.total_duration_ms == 250


def test_build_run_timeline_empty(db_session) -> None:
    """A run with no events returns None."""
    session, conv_id = db_session
    run = create_run(session, conversation_id=conv_id)
    assert build_run_timeline(session, run.id) is None


def test_compare_runs(db_session) -> None:
    """Comparison returns correct metric deltas."""
    session, conv_id = db_session
    run_a = create_run(session, conversation_id=conv_id, model="m")
    finish_run(session, run_a.id, finish_reason="stop", usage={"total_tokens": 100, "cost_usd": 0.01}, iterations=2)

    run_b = create_run(session, conversation_id=conv_id, model="m")
    finish_run(session, run_b.id, finish_reason="stop", usage={"total_tokens": 150, "cost_usd": 0.02}, iterations=3)

    # Seed minimal events for timelines.
    append_run_events(session, run_id=run_a.id, events=[
        ("llm_call_complete", {"iteration": 1, "model": "m", "usage": {"total_tokens": 100}, "duration_ms": 200}),
        ("finish", {"reason": "stop", "elapsed_ms": 200}),
    ])
    append_run_events(session, run_id=run_b.id, events=[
        ("llm_call_complete", {"iteration": 1, "model": "m", "usage": {"total_tokens": 80}, "duration_ms": 100}),
        ("llm_call_complete", {"iteration": 2, "model": "m", "usage": {"total_tokens": 70}, "duration_ms": 120}),
        ("finish", {"reason": "stop", "elapsed_ms": 220}),
    ])

    result = compare_runs(session, run_a.id, run_b.id)
    assert result is not None
    assert result.delta_tokens == 50  # 150 - 100
    assert result.delta_cost_usd == pytest.approx(0.01)  # 0.02 - 0.01
    assert result.delta_iterations == 1  # 3 - 2
    assert len(result.iterations_a) == 1
    assert len(result.iterations_b) == 2


def test_prepare_replay(db_session) -> None:
    """Replay context extracts the original user input and config."""
    session, conv_id = db_session
    from app.agent.service import append_message

    append_message(session, conversation_id=conv_id, role="user", content="What is 2+2?")
    run = create_run(session, conversation_id=conv_id, model="gpt-4o", config={"tool_names": ["read_file"]})
    finish_run(session, run.id, finish_reason="stop", iterations=1)

    ctx = prepare_replay(session, run.id, model_override="claude-3")
    assert ctx is not None
    assert ctx.user_input == "What is 2+2?"
    assert ctx.model == "claude-3"  # override applied
    assert ctx.conversation_id == conv_id
    assert ctx.tool_names == ["read_file"]


# --- API: timeline + compare endpoints -------------------------------------


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    monkeypatch.setattr("app.providers.get_provider_for_model", lambda model=None: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_provider_for_model", lambda model=None: provider)


def test_timeline_api_endpoint(monkeypatch) -> None:
    """GET /conversations/{id}/runs/{id}/timeline returns structured data."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["Hello."])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={"title": "insp"}).json()["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "hi"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for _ in resp.iter_lines():
                pass

        runs = c.get(f"/api/conversations/{conv_id}/runs").json()
        assert len(runs) == 1
        run_id = runs[0]["id"]

        timeline = c.get(f"/api/conversations/{conv_id}/runs/{run_id}/timeline").json()
        assert timeline["run"]["id"] == run_id
        assert len(timeline["iterations"]) >= 1
        assert timeline["iterations"][0]["iteration"] == 1
        assert timeline["iterations"][0]["duration_ms"] is not None
        # model may be None when the conversation has no explicit model configured
        assert "model" in timeline["iterations"][0]


def test_compare_api_endpoint(monkeypatch) -> None:
    """GET /runs/compare?a=X&b=Y returns metric deltas."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["First.", "Second."])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={}).json()["id"]
        # Create two runs.
        for msg in ["one", "two"]:
            with c.stream(
                "POST",
                f"/api/conversations/{conv_id}/messages",
                json={"content": msg},
                headers={"Accept": "text/event-stream"},
            ) as resp:
                for _ in resp.iter_lines():
                    pass

        runs = c.get(f"/api/conversations/{conv_id}/runs").json()
        assert len(runs) == 2
        a_id, b_id = runs[1]["id"], runs[0]["id"]  # oldest first

        comparison = c.get(f"/api/runs/compare?a={a_id}&b={b_id}").json()
        assert comparison["run_a"]["id"] == a_id
        assert comparison["run_b"]["id"] == b_id
        assert "delta_tokens" in comparison
        assert "delta_iterations" in comparison
        assert "iterations_a" in comparison
        assert "iterations_b" in comparison


def test_compare_api_404_missing_run(monkeypatch) -> None:
    """Comparing with a non-existent run returns 404."""
    from app.main import app

    with TestClient(app) as c:
        resp = c.get("/api/runs/compare?a=99999&b=99998")
        assert resp.status_code == 404


def test_replay_endpoint(monkeypatch) -> None:
    """POST .../runs/{id}/replay creates a new queued run."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["Answer."])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={}).json()["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "original question"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for _ in resp.iter_lines():
                pass

        runs = c.get(f"/api/conversations/{conv_id}/runs").json()
        run_id = runs[0]["id"]

        replay_resp = c.post(
            f"/api/conversations/{conv_id}/runs/{run_id}/replay",
            json={"model": "other-model"},
        )
        assert replay_resp.status_code == 200
        data = replay_resp.json()
        assert data["original_run_id"] == run_id
        assert data["new_run_id"] != run_id
        assert data["status"] == "queued"


# --- InspectorRegistry unit tests ------------------------------------------


def test_inspector_registry_pubsub() -> None:
    """Subscribe, publish, receive, notify_finished, sentinel."""
    reg = InspectorRegistry()
    q = reg.subscribe(1)
    assert reg.has_subscribers(1)

    reg.publish(1, {"kind": "token", "payload": {"text": "hi"}})
    reg.publish(1, {"kind": "finish", "payload": {"reason": "stop"}})
    reg.notify_finished(1)

    assert q.get_nowait() == {"kind": "token", "payload": {"text": "hi"}}
    assert q.get_nowait() == {"kind": "finish", "payload": {"reason": "stop"}}
    assert q.get_nowait() is None  # sentinel
    assert not reg.has_subscribers(1)


def test_inspector_registry_unsubscribe() -> None:
    """Unsubscribing removes the queue; further publishes are no-ops."""
    reg = InspectorRegistry()
    q = reg.subscribe(42)
    reg.unsubscribe(42, q)
    assert not reg.has_subscribers(42)
    # Publishing after unsubscribe doesn't raise.
    reg.publish(42, {"kind": "token", "payload": {}})
    assert q.empty()


def test_inspector_registry_multiple_subscribers() -> None:
    """Multiple subscribers each receive all events."""
    reg = InspectorRegistry()
    q1 = reg.subscribe(7)
    q2 = reg.subscribe(7)

    reg.publish(7, {"kind": "start", "payload": {}})
    reg.notify_finished(7)

    assert q1.get_nowait() == {"kind": "start", "payload": {}}
    assert q1.get_nowait() is None
    assert q2.get_nowait() == {"kind": "start", "payload": {}}
    assert q2.get_nowait() is None
