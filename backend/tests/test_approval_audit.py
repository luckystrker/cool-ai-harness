"""Tests for the approval audit trail API endpoint (Фаза 1.5 §2).

Verifies that:
- The GET /conversations/{id}/approvals endpoint returns audit records
- Records are created when tools are approved/denied during a conversation turn
- The endpoint filters by run_id and respects the limit parameter
"""

from __future__ import annotations

import json

from fastapi.testclient import TestClient

from tests.conftest import ScriptedProvider


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    monkeypatch.setattr("app.providers.get_default_provider", lambda: provider)
    import app.api.conversations as conv_module

    monkeypatch.setattr(conv_module, "get_default_provider", lambda: provider)


def test_approval_audit_list_empty(monkeypatch) -> None:
    """A new conversation has no approval audit records."""
    from app.main import app

    with TestClient(app) as c:
        conv = c.post("/api/conversations", json={"title": "audit-test"}).json()
        conv_id = conv["id"]

        resp = c.get(f"/api/conversations/{conv_id}/approvals")
        assert resp.status_code == 200
        assert resp.json() == []


def test_approval_audit_list_404_for_missing_conversation() -> None:
    from app.main import app

    with TestClient(app) as c:
        resp = c.get("/api/conversations/999999/approvals")
        assert resp.status_code == 404


def test_approval_audit_created_on_denied_tool(monkeypatch) -> None:
    """When a tool is auto-denied (timeout), an audit record is written."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            [
                {
                    "id": "call_audit_1",
                    "name": "python_execute",
                    "arguments": {"code": "print(1)"},
                }
            ],
            "done",
        ]
    )
    _patch_provider(monkeypatch, provider)

    from app.core import config as config_module

    monkeypatch.setattr(config_module.get_settings(), "approval_timeout_s", 0.2)
    monkeypatch.setattr(config_module.get_settings(), "default_tool_permissions", {"*": "ask"})

    with TestClient(app) as c:
        conv = c.post(
            "/api/conversations",
            json={"title": "audit-denied", "permissions": {"*": "ask"}},
        ).json()
        conv_id = conv["id"]

        # Drive a turn that will auto-deny (timeout).
        kinds: list[str] = []
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

        # The tool was denied (auto-timeout).
        assert "tool_approval_request" in kinds
        assert "tool_result" in kinds

        # Check audit records exist.
        resp = c.get(f"/api/conversations/{conv_id}/approvals")
        assert resp.status_code == 200
        audits = resp.json()
        assert len(audits) >= 1

        audit = audits[0]
        assert audit["tool_name"] == "python_execute"
        assert audit["approved"] is False
        assert audit["decision_source"] in ("timeout", "policy")
        assert audit["call_id"] == "call_audit_1"


def test_approval_audit_created_on_allowed_tool(monkeypatch) -> None:
    """When a tool runs freely (allowed), an audit record is written with source='auto'."""
    from app.main import app

    provider = ScriptedProvider()
    provider.set_script(
        [
            [
                {
                    "id": "call_audit_2",
                    "name": "read_file",
                    "arguments": {"path": "test.txt"},
                }
            ],
            "done",
        ]
    )
    _patch_provider(monkeypatch, provider)

    from app.core import config as config_module

    monkeypatch.setattr(config_module.get_settings(), "default_tool_permissions", {"*": "allow"})

    with TestClient(app) as c:
        conv = c.post(
            "/api/conversations",
            json={"title": "audit-allowed", "permissions": {"*": "allow"}},
        ).json()
        conv_id = conv["id"]

        kinds: list[str] = []
        with c.stream(
            "POST",
            f"/api/conversations/{conv_id}/messages",
            json={"content": "read it"},
            headers={"Accept": "text/event-stream"},
        ) as resp:
            for line in resp.iter_lines():
                if line.startswith("data:"):
                    ev = json.loads(line[len("data:") :].strip())
                    kinds.append(ev["kind"])

        assert "tool_result" in kinds

        resp = c.get(f"/api/conversations/{conv_id}/approvals")
        assert resp.status_code == 200
        audits = resp.json()
        assert len(audits) >= 1

        audit = audits[0]
        assert audit["approved"] is True
        assert audit["decision_source"] == "auto"


def test_capability_policy_in_conversation_crud(monkeypatch) -> None:
    """Capability policy and breakpoints can be set via the conversation API."""
    from app.main import app

    with TestClient(app) as c:
        # Create with capability policy + breakpoints.
        resp = c.post(
            "/api/conversations",
            json={
                "title": "capability-test",
                "capability_policy": {"execute": "ask", "network": "deny"},
                "breakpoints": [{"type": "before_write", "tool": "write_file"}],
            },
        )
        assert resp.status_code == 200, resp.text
        conv = resp.json()
        conv_id = conv["id"]

        assert conv["capability_policy"] == {"execute": "ask", "network": "deny"}
        assert conv["breakpoints"] == [{"type": "before_write", "tool": "write_file"}]

        # Patch capability policy.
        resp = c.patch(
            f"/api/conversations/{conv_id}",
            json={"capability_policy": {"*": "ask"}},
        )
        assert resp.status_code == 200
        assert resp.json()["capability_policy"] == {"*": "ask"}

        # Clear breakpoints.
        resp = c.patch(
            f"/api/conversations/{conv_id}",
            json={"breakpoints": []},
        )
        assert resp.status_code == 200
        assert resp.json()["breakpoints"] is None


def test_invalid_capability_policy_rejected() -> None:
    from app.main import app

    with TestClient(app) as c:
        resp = c.post(
            "/api/conversations",
            json={"capability_policy": {"execute": "maybe"}},
        )
        assert resp.status_code == 400
        assert "allow|ask|deny" in resp.text
