"""Tests for the analytics aggregation service and API (Фаза 3a §5).

Covers:
  - SpendLog aggregation (spend_over_time, spend_by_model)
  - ToolCall aggregation (top_tools)
  - LLM latency from RunEvent (llm_latency)
  - Unified call history (call_history)
  - Memory activity timeseries (memory_activity)
  - Summary stats
  - API endpoint integration
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session, SQLModel, create_engine

from app.analytics import (
    call_history,
    llm_latency,
    memory_activity,
    spend_by_model,
    spend_over_time,
    summary_stats,
    top_tools,
)
from app.models import Conversation, SpendLog, ToolCall, User


@pytest.fixture()
def db_session():
    """An isolated in-memory SQLite session for analytics tests."""
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False})
    SQLModel.metadata.create_all(engine)
    with Session(engine) as session:
        user = User(username="analytics_test")
        session.add(user)
        session.commit()
        session.refresh(user)
        conv = Conversation(user_id=user.id, title="test")
        session.add(conv)
        session.commit()
        session.refresh(conv)
        yield session, user.id, conv.id


def _seed_spend(session: Session, user_id: int, conv_id: int) -> None:
    """Seed SpendLog with test data across two days and two models."""
    now = datetime.now(UTC)
    yesterday = now - timedelta(days=1)

    rows = [
        SpendLog(
            user_id=user_id, conversation_id=conv_id, provider_name="openai",
            model="gpt-4o", prompt_tokens=100, completion_tokens=50,
            total_tokens=150, cost_usd=0.01, ts=yesterday,
        ),
        SpendLog(
            user_id=user_id, conversation_id=conv_id, provider_name="openai",
            model="gpt-4o", prompt_tokens=200, completion_tokens=100,
            total_tokens=300, cost_usd=0.02, ts=yesterday,
        ),
        SpendLog(
            user_id=user_id, conversation_id=conv_id, provider_name="anthropic",
            model="claude-sonnet-4-20250514", prompt_tokens=50, completion_tokens=25,
            total_tokens=75, cost_usd=0.005, ts=now,
        ),
    ]
    for r in rows:
        session.add(r)
    session.commit()


def _seed_tool_calls(session: Session, user_id: int, conv_id: int) -> None:
    """Seed ToolCall with test data."""
    rows = [
        ToolCall(
            conversation_id=conv_id, user_id=user_id, name="read_file",
            arguments={"path": "x.txt"}, result={"output": "ok"},
            duration_ms=10, success=True,
        ),
        ToolCall(
            conversation_id=conv_id, user_id=user_id, name="read_file",
            arguments={"path": "y.txt"}, result={"output": "ok"},
            duration_ms=20, success=True,
        ),
        ToolCall(
            conversation_id=conv_id, user_id=user_id, name="write_file",
            arguments={"path": "z.txt"}, result={"output": "err"},
            duration_ms=50, success=False, error="permission denied",
        ),
    ]
    for r in rows:
        session.add(r)
    session.commit()


def _seed_run_events(session: Session, conv_id: int) -> None:
    """Seed RunEvent with llm_call_complete events for latency tests."""
    from app.agent.service import create_run

    run = create_run(session, conversation_id=conv_id, model="gpt-4o")
    events = [
        ("llm_call_complete", {"iteration": 1, "model": "gpt-4o", "duration_ms": 100, "usage": {"total_tokens": 15}}),
        ("llm_call_complete", {"iteration": 2, "model": "gpt-4o", "duration_ms": 200, "usage": {"total_tokens": 20}}),
    ]
    from app.agent.service import append_run_events

    append_run_events(session, run_id=run.id, events=events)


# --- Service-level tests ---


def test_spend_over_time(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    result = spend_over_time(session, days=7, bucket="day")
    assert len(result) >= 1
    # Total cost should be 0.035
    total = sum(r["cost_usd"] for r in result)
    assert abs(total - 0.035) < 0.001


def test_spend_over_time_hourly_bucket(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    result = spend_over_time(session, days=7, bucket="hour")
    assert len(result) >= 1
    total = sum(r["cost_usd"] for r in result)
    assert abs(total - 0.035) < 0.001


def test_spend_by_model(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    result = spend_by_model(session, days=7)
    assert len(result) == 2
    # gpt-4o should be first (higher spend)
    assert result[0]["model"] == "gpt-4o"
    assert abs(result[0]["cost_usd"] - 0.03) < 0.001
    assert result[0]["calls"] == 2
    assert result[1]["model"] == "claude-sonnet-4-20250514"
    assert result[1]["calls"] == 1


def test_top_tools(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_tool_calls(session, user_id, conv_id)

    result = top_tools(session, days=7)
    assert len(result) == 2
    # read_file has 2 calls, write_file has 1
    assert result[0]["name"] == "read_file"
    assert result[0]["calls"] == 2
    assert result[0]["success_rate"] == 1.0
    assert result[0]["error_count"] == 0
    assert result[1]["name"] == "write_file"
    assert result[1]["calls"] == 1
    assert result[1]["success_rate"] == 0.0
    assert result[1]["error_count"] == 1


def test_top_tools_avg_duration(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_tool_calls(session, user_id, conv_id)

    result = top_tools(session, days=7)
    read_file = next(r for r in result if r["name"] == "read_file")
    # avg of 10 and 20 = 15
    assert abs(read_file["avg_duration_ms"] - 15.0) < 0.1


def test_llm_latency(db_session) -> None:
    session, _user_id, conv_id = db_session
    _seed_run_events(session, conv_id)

    result = llm_latency(session, days=7)
    assert len(result) >= 1
    # avg of 100 and 200 = 150
    total_calls = sum(r["calls"] for r in result)
    assert total_calls == 2
    avg = sum(r["avg_ms"] * r["calls"] for r in result) / total_calls
    assert abs(avg - 150.0) < 1.0


def test_call_history(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    rows, total = call_history(session, limit=10, offset=0)
    assert total == 3
    assert len(rows) == 3
    # Newest first
    assert rows[0]["model"] == "claude-sonnet-4-20250514"


def test_call_history_filter_model(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    rows, total = call_history(session, model="gpt-4o")
    assert total == 2
    assert all(r["model"] == "gpt-4o" for r in rows)


def test_call_history_filter_provider(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    rows, total = call_history(session, provider="anthropic")
    assert total == 1
    assert rows[0]["provider_name"] == "anthropic"


def test_call_history_pagination(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)

    rows, total = call_history(session, limit=2, offset=0)
    assert total == 3
    assert len(rows) == 2

    rows2, _ = call_history(session, limit=2, offset=2)
    assert len(rows2) == 1


def test_memory_activity(db_session) -> None:
    session, user_id, _conv_id = db_session
    from app.memory.models import MemoryItem

    items = [
        MemoryItem(user_id=user_id, memory_type="semantic", content="fact 1"),
        MemoryItem(user_id=user_id, memory_type="semantic", content="fact 2"),
        MemoryItem(user_id=user_id, memory_type="episodic", content="episode 1"),
    ]
    for item in items:
        session.add(item)
    session.commit()

    result = memory_activity(session, days=7)
    assert len(result) >= 1
    total_created = sum(r["created"] for r in result)
    assert total_created == 3
    # Check by_type breakdown
    all_types: dict[str, int] = {}
    for r in result:
        for k, v in r["by_type"].items():
            all_types[k] = all_types.get(k, 0) + v
    assert all_types.get("semantic") == 2
    assert all_types.get("episodic") == 1


def test_summary_stats(db_session) -> None:
    session, user_id, conv_id = db_session
    _seed_spend(session, user_id, conv_id)
    _seed_tool_calls(session, user_id, conv_id)

    stats = summary_stats(session, days=7)
    assert abs(stats["total_spend_usd"] - 0.035) < 0.001
    assert stats["total_llm_calls"] == 3
    assert stats["total_tokens"] == 525  # 150 + 300 + 75
    assert stats["total_tool_calls"] == 3
    assert stats["tool_error_count"] == 1
    assert abs(stats["tool_success_rate"] - 0.667) < 0.01


def test_summary_stats_empty(db_session) -> None:
    session, _user_id, _conv_id = db_session
    stats = summary_stats(session, days=7)
    assert stats["total_spend_usd"] == 0.0
    assert stats["total_llm_calls"] == 0
    assert stats["total_tool_calls"] == 0
    assert stats["tool_success_rate"] == 1.0


# --- API integration tests ---


@pytest.fixture()
def client():
    """TestClient for API-level tests."""
    from app.main import create_app

    app = create_app()
    return TestClient(app)


def test_api_summary(client) -> None:
    resp = client.get("/api/analytics/summary?days=7")
    assert resp.status_code == 200
    data = resp.json()
    assert "total_spend_usd" in data
    assert "total_llm_calls" in data
    assert "days" in data


def test_api_spend_over_time(client) -> None:
    resp = client.get("/api/analytics/spend-over-time?days=7&bucket=day")
    assert resp.status_code == 200
    assert isinstance(resp.json(), list)


def test_api_spend_by_model(client) -> None:
    resp = client.get("/api/analytics/spend-by-model?days=7")
    assert resp.status_code == 200
    assert isinstance(resp.json(), list)


def test_api_top_tools(client) -> None:
    resp = client.get("/api/analytics/top-tools?days=7")
    assert resp.status_code == 200
    assert isinstance(resp.json(), list)


def test_api_latency(client) -> None:
    resp = client.get("/api/analytics/latency?days=7")
    assert resp.status_code == 200
    assert isinstance(resp.json(), list)


def test_api_call_history(client) -> None:
    resp = client.get("/api/analytics/call-history?limit=10")
    assert resp.status_code == 200
    data = resp.json()
    assert "rows" in data
    assert "total" in data


def test_api_memory_activity(client) -> None:
    resp = client.get("/api/analytics/memory-activity?days=7")
    assert resp.status_code == 200
    assert isinstance(resp.json(), list)


def test_api_invalid_bucket(client) -> None:
    resp = client.get("/api/analytics/spend-over-time?bucket=week")
    assert resp.status_code == 422
