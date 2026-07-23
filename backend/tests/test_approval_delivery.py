"""Probe: does a tool_approval_request event actually travel over the SSE stream?

This reproduces the "dialog never appears" symptom end-to-end: it drives a turn
through the real HTTP SSE endpoint with a tool that resolves to "ask", and
asserts the approval event is among the streamed events BEFORE the turn blocks.

If this test sees tool_call_start but not tool_approval_request, the issue is in
delivery / event ordering on the backend side. If it sees both, the bug is in
the frontend rendering.
"""

from __future__ import annotations

import json

from fastapi.testclient import TestClient

from tests.conftest import ScriptedProvider


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_default_provider", lambda: provider)


def test_approval_request_is_streamed_before_blocking(monkeypatch) -> None:
    from app.main import app

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

    # Force a short approval timeout so the turn auto-denies and the stream
    # closes within the test instead of hanging for 30s.
    from app.core import config as config_module

    monkeypatch.setattr(config_module.get_settings(), "approval_timeout_s", 0.2)

    with TestClient(app) as c:
        conv = c.post("/api/conversations", json={"title": "approval"}).json()
        conv_id = conv["id"]

        kinds: list[str] = []
        # Use the non-streaming .stream() context so we drain the SSE body.
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "run it"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for line in resp.iter_lines():
                if line.startswith("data:"):
                    ev = json.loads(line[len("data:") :].strip())
                    kinds.append(ev["kind"])

    # The crucial assertion: the approval request must reach the client.
    assert "tool_call_start" in kinds
    assert "tool_approval_request" in kinds, (
        f"approval request not streamed! kinds seen: {kinds}"
    )
    # The turn must terminate (auto-denied via timeout) rather than hang forever.
    assert "tool_result" in kinds
    assert "finish" in kinds
