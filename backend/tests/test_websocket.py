"""Tests for the WebSocket chat endpoint.

Uses a ScriptedProvider via monkeypatch on ``app.providers.get_default_provider``
so no real network calls are made.
"""

from __future__ import annotations

import json

from fastapi.testclient import TestClient

from tests.conftest import ScriptedProvider


def test_ws_chat_round_trip(monkeypatch) -> None:
    """A user message over the socket should stream back token + finish events."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(["Hello from the test."])
    # Inject our provider into the module the WS handler imports.
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    # And the symbol already imported into app.api.websocket's namespace.
    import app.api.websocket as ws_module

    monkeypatch.setattr(ws_module, "get_default_provider", lambda: provider)

    with TestClient(app) as c:
        # Create a conversation first via the HTTP API.
        conv = c.post("/api/conversations", json={"title": "WS"}).json()
        conv_id = conv["id"]

        with c.websocket_connect(f"/ws/chat/{conv_id}") as ws:
            ws.send_text(json.dumps({"content": "say hi"}))
            # Collect events until we see finish.
            events = []
            while True:
                raw = ws.receive_text()
                ev = json.loads(raw)
                events.append(ev)
                if ev["kind"] == "finish":
                    break

    kinds = [e["kind"] for e in events]
    assert kinds[0] == "start"
    assert "token" in kinds
    assert kinds[-1] == "finish"

    text = "".join(e["payload"]["text"] for e in events if e["kind"] == "token")
    assert "Hello from the test" in text


def test_ws_invalid_message_returns_error(monkeypatch) -> None:
    """Malformed incoming frames should produce an ``error`` event, not close."""
    from app.main import app

    provider = ScriptedProvider()
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    import app.api.websocket as ws_module

    monkeypatch.setattr(ws_module, "get_default_provider", lambda: provider)

    with TestClient(app) as c:
        conv = c.post("/api/conversations", json={"title": "WS-err"}).json()
        conv_id = conv["id"]

        with c.websocket_connect(f"/ws/chat/{conv_id}") as ws:
            # Missing required `content` field.
            ws.send_text(json.dumps({"foo": "bar"}))
            raw = ws.receive_text()
            ev = json.loads(raw)
            assert ev["kind"] == "error"
            assert "content" in ev["payload"]["message"].lower() or "invalid" in ev["payload"]["message"].lower()
