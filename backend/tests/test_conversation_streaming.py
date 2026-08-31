"""Integration tests for the SSE streaming + message persistence.

Exercises the full ``POST /conversations/{id}/messages`` → runner → executor →
persistence path with a ScriptedProvider, so we can assert on what actually
lands in the database after a multi-turn (tool-calling) run.
"""

from __future__ import annotations

import json
import threading
import time
from contextlib import suppress

from fastapi.testclient import TestClient

from tests.conftest import ScriptedProvider


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    """Inject a provider into every module that resolves the provider."""
    monkeypatch.setattr("app.providers.get_provider_for_model", lambda model=None: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_provider_for_model", lambda model=None: provider)


def test_tool_round_trip_persists_both_assistant_messages(monkeypatch) -> None:
    """Regression: a tool round-trip must persist TWO assistant rows.

    Before the fix, the runner only wrote the *last* assistant message on
    `finish`, so the assistant turn that requested the tool (carrying
    ``tool_calls``) was overwritten by the follow-up text answer and lost.
    The UI then had no tool-call block to render, making the tool call
    invisible (seen on conversation #54).
    """
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            # Turn 1: model asks to call python_execute (2+2).
            [
                {
                    "id": "call_1",
                    "name": "python_execute",
                    "arguments": {"code": "print(2 + 2)"},
                }
            ],
            # Turn 2: final text answer after seeing the tool result.
            "2 + 2 = 4",
        ]
    )
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv = c.post(
            "/api/conversations",
            json={"title": "tool-rt", "permissions": {"*": "allow"}},
        ).json()
        conv_id = conv["id"]

        # Drive the turn over the SSE endpoint.
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "посчитай 2+2"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            kinds = []
            for line in resp.iter_lines():
                if line.startswith("data:"):
                    payload = line[len("data:") :].strip()
                    ev = json.loads(payload)
                    kinds.append(ev["kind"])

        assert "tool_call_start" in kinds
        assert "tool_result" in kinds
        assert "finish" in kinds

        # Inspect the persisted history.
        detail = c.get(f"/api/conversations/{conv_id}").json()
        msgs = detail["messages"]

    roles = [m["role"] for m in msgs]
    # Expected: user, assistant(tool_calls), tool, assistant(text).
    assert roles == ["user", "assistant", "tool", "assistant"], roles

    # The FIRST assistant message must carry the tool call request.
    first_assistant = msgs[1]
    assert first_assistant["tool_calls"], "tool_calls were dropped from the requesting turn"
    assert first_assistant["tool_calls"][0]["name"] == "python_execute"

    # The tool row carries the structured result so the UI can render it.
    tool_msg = msgs[2]
    assert tool_msg["role"] == "tool"
    assert tool_msg["tool_result"]["name"] == "python_execute"
    assert tool_msg["tool_result"]["tool_call_id"] == "call_1"

    # The SECOND assistant message is the final text answer (no tool calls).
    final = msgs[3]
    assert "4" in final["content"]
    assert not final["tool_calls"]


def test_thinking_persisted_on_assistant_message(monkeypatch) -> None:
    """Reasoning carried on the `message` event must be saved as `thinking`."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script([{"reasoning": "I should answer politely.", "text": "Hi!"}])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv = c.post("/api/conversations", json={"title": "thinking"}).json()
        conv_id = conv["id"]

        c.post(
            f"/api/conversations/{conv_id}/messages",
            json={"content": "hello"},
            headers={"Accept": "text/event-stream"},
        )

        detail = c.get(f"/api/conversations/{conv_id}").json()
        msgs = detail["messages"]

    assistants = [m for m in msgs if m["role"] == "assistant"]
    assert assistants, "expected an assistant message"
    assert assistants[0]["thinking"] is not None
    assert "answer politely" in assistants[0]["thinking"]


def test_fast_http_approval_resumes_live_sse_turn(monkeypatch) -> None:
    """The real async approval route must safely win the post-yield race."""
    from sqlmodel import Session

    from app.agent.approvals import approval_registry
    from app.agent.service import list_runs
    from app.core.db import engine
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            [
                {
                    "id": "same-model-call",
                    "name": "python_execute",
                    "arguments": {"code": "print(1)"},
                }
            ],
            "done",
        ]
    )
    _patch_provider(monkeypatch, provider)

    kinds: list[str] = []
    streamed_events: list[dict] = []
    failures: list[BaseException] = []

    with TestClient(app) as client:
        conv = client.post(
            "/api/conversations",
            json={"title": "http-approval", "permissions": {"*": "ask"}},
        ).json()
        conv_id = conv["id"]

        def _drive_stream() -> None:
            try:
                with client.stream(
                    "POST",
                    f"/api/conversations/{conv_id}/messages",
                    json={"content": "run it"},
                    headers={"Accept": "text/event-stream"},
                ) as response:
                    for line in response.iter_lines():
                        if line.startswith("data:"):
                            event = json.loads(line[len("data:") :].strip())
                            streamed_events.append(event)
                            kinds.append(event["kind"])
            except BaseException as exc:  # pragma: no cover - asserted in the parent thread
                failures.append(exc)

        thread = threading.Thread(target=_drive_stream, daemon=True)
        thread.start()

        ticket = None
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and ticket is None:
            with Session(engine) as session:
                runs = list_runs(session, conversation_id=conv_id)
                run_id = runs[0].id if runs else None
            if run_id is not None:
                with suppress(KeyError):
                    ticket = approval_registry.ticket(
                        "same-model-call",
                        actor_id=conv["user_id"],
                        conversation_id=conv_id,
                        run_id=run_id,
                    )
            if ticket is None:
                time.sleep(0.01)

        assert ticket is not None, "live SSE turn did not register an approval"
        approval = client.post(
            f"/api/conversations/{conv_id}/tool_calls/{ticket.approval_id}/approval",
            json={
                "approved": True,
                "expected_revision": ticket.revision,
                "run_id": run_id,
            },
        )
        assert approval.status_code == 200, approval.text

        thread.join(timeout=5)
        assert not thread.is_alive(), "SSE turn did not resume after HTTP approval"

    assert failures == []
    assert "tool_approval_request" in kinds
    assert "tool_approval_resolved" in kinds
    resolved = next(event for event in streamed_events if event["kind"] == "tool_approval_resolved")
    assert resolved["payload"]["approval_id"] == ticket.approval_id
    assert resolved["payload"]["decision"] == "approved"
    assert "tool_result" in kinds
    assert "finish" in kinds
