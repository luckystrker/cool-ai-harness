"""Tests for per-message model + turn duration metadata (p.4).

Drives a real run via the SSE endpoint with a ScriptedProvider and asserts the
final assistant message is stamped with the model id and a whole-turn duration.
"""

from __future__ import annotations

import json

from fastapi.testclient import TestClient

from tests.conftest import ScriptedProvider


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_default_provider", lambda: provider)


def test_final_assistant_message_carries_model_and_duration(monkeypatch) -> None:
    """The finish event stamps model + duration_ms on the last assistant row."""
    from app.main import app

    provider = ScriptedProvider(default_model="scripted-model")
    provider.set_script(["hello world"])
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv = c.post("/api/conversations", json={"model": "scripted-model"}).json()
        conv_id = conv["id"]

        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "hi"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for line in resp.iter_lines():
                if line.startswith("data:"):
                    json.loads(line[len("data:") :].strip())

        detail = c.get(f"/api/conversations/{conv_id}").json()
        msgs = detail["messages"]

    roles = [m["role"] for m in msgs]
    assert roles == ["user", "assistant"], roles

    assistant = msgs[1]
    assert assistant["model"] == "scripted-model"
    assert isinstance(assistant["duration_ms"], int) and assistant["duration_ms"] >= 0
    # created_at is present on every message (send timestamp).
    assert assistant["created_at"]
    assert msgs[0]["created_at"]


def test_user_and_tool_messages_have_no_model(monkeypatch) -> None:
    """Only assistant messages get the model stamp; user/tool stay None."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            [{"id": "c1", "name": "python_execute", "arguments": {"code": "1"}}],
            "done",
        ]
    )
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        conv = c.post("/api/conversations", json={"model": "m"}).json()
        conv_id = conv["id"]
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "run"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for line in resp.iter_lines():
                if line.startswith("data:"):
                    json.loads(line[len("data:") :].strip())
        msgs = c.get(f"/api/conversations/{conv_id}").json()["messages"]

    by_role = {m["role"]: m for m in msgs}
    assert by_role["user"]["model"] is None
    assert by_role["tool"]["model"] is None
    assert by_role["tool"]["duration_ms"] is None
