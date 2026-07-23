"""Tests for persistent tool-call logging (Фаза 1 tail → Фаза 3a prep).

The runner must write one ``tool_calls`` row per tool invocation, capturing
name / arguments / result / duration / success — so observability (Фаза 3a) has
data to show. This lives alongside the existing ``messages``-row persistence;
both are asserted so the contract is explicit.

Conversations are created with ``permissions={"*": "allow"}`` so tool calls run
straight through — otherwise the default "ask" fallback would block each call
on an approval that nobody resolves (and the test would hang until the timeout).
"""

from __future__ import annotations

import json

from fastapi.testclient import TestClient
from sqlmodel import Session, select

from app.core.db import engine
from app.models import ToolCall as ToolCallRow
from tests.conftest import ScriptedProvider

# Allow-all policy so tests don't block on the approval gate.
_ALLOW_ALL = {"permissions": {"*": "allow"}}


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_default_provider", lambda: provider)


def _drain(resp) -> list[dict]:
    kinds: list[dict] = []
    for line in resp.iter_lines():
        if line.startswith("data:"):
            kinds.append(json.loads(line[len("data:") :].strip()))
    return kinds


def test_tool_invocation_persists_tool_calls_row(monkeypatch) -> None:
    """A successful tool call writes exactly one tool_calls observability row
    with the arguments, result, duration, and success=True."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            [
                {
                    "id": "call_1",
                    "name": "python_execute",
                    "arguments": {"code": "print(2 + 2)"},
                }
            ],
            "2 + 2 = 4",
        ]
    )
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={"title": "log", **_ALLOW_ALL}).json()["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "посчитай 2+2"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            events = _drain(resp)

        assert any(e["kind"] == "tool_result" for e in events), "tool never ran"

        rows: list[ToolCallRow]
        with Session(engine) as s:
            rows = list(s.exec(select(ToolCallRow).where(ToolCallRow.conversation_id == conv_id)))

    assert len(rows) == 1, f"expected 1 tool_calls row, got {len(rows)}"
    row = rows[0]
    assert row.conversation_id == conv_id
    assert row.name == "python_execute"
    # Arguments captured from tool_call_start, not from the result.
    assert row.arguments == {"code": "print(2 + 2)"}
    # The structured result payload is stored for later inspection.
    assert row.result is not None
    assert row.result["is_error"] is False
    assert row.success is True
    assert row.error is None
    # duration_ms is populated by the executor.
    assert row.duration_ms is not None
    assert row.duration_ms >= 0


def test_failed_tool_call_still_logs_with_success_false(monkeypatch) -> None:
    """An unknown tool still produces a tool_calls row, marked failed."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            [{"id": "c1", "name": "does_not_exist", "arguments": {}}],
            "Recovered.",
        ]
    )
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={"title": "fail", **_ALLOW_ALL}).json()["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "go"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            events = _drain(resp)

        assert any(e["kind"] == "tool_result" for e in events)

        with Session(engine) as s:
            rows = list(s.exec(select(ToolCallRow).where(ToolCallRow.conversation_id == conv_id)))

    assert len(rows) == 1
    row = rows[0]
    assert row.name == "does_not_exist"
    assert row.success is False
    assert row.error is not None
    assert "Unknown tool" in row.error


def test_plain_text_turn_logs_no_tool_calls(monkeypatch) -> None:
    """A turn without any tool call must not create tool_calls rows."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["just text, no tools"])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={"title": "plain", **_ALLOW_ALL}).json()["id"]
        c.post(
            f"/api/conversations/{conv_id}/messages",
            json={"content": "hi"},
            headers={"Accept": "text/event-stream"},
        )
        with Session(engine) as s:
            rows = list(s.exec(select(ToolCallRow).where(ToolCallRow.conversation_id == conv_id)))

    assert rows == []


def test_tool_calls_row_links_to_requesting_assistant_message(monkeypatch) -> None:
    """The tool_calls row should reference the assistant message that requested
    the call (message_id), so the observability view can group them."""
    from app.main import app
    from app.models import Message as MessageRow

    provider = ScriptedProvider()
    provider.set_script(
        [
            [
                {
                    "id": "call_1",
                    "name": "python_execute",
                    "arguments": {"code": "print(1)"},
                }
            ],
            "done",
        ]
    )
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv_id = c.post("/api/conversations", json={"title": "link", **_ALLOW_ALL}).json()["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "run"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            _drain(resp)

        with Session(engine) as s:
            all_assistants = list(
                s.exec(
                    select(MessageRow)
                    .where(MessageRow.conversation_id == conv_id)
                    .where(MessageRow.role == "assistant")
                )
            )
            # The assistant turn that requested the tool is the one with a
            # non-empty tool_calls list (the follow-up turn has tool_calls=None).
            assistants = [m for m in all_assistants if m.tool_calls]
            tc_rows = list(
                s.exec(select(ToolCallRow).where(ToolCallRow.conversation_id == conv_id))
            )

    # There is exactly one assistant message that requested a tool, and the
    # tool_calls row points at it.
    assert len(assistants) == 1
    assert len(tc_rows) == 1
    assert tc_rows[0].message_id == assistants[0].id
