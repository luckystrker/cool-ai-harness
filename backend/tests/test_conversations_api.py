"""Tests for the conversations API: CRUD endpoints."""

from __future__ import annotations

from fastapi.testclient import TestClient


def _client() -> TestClient:
    from app.main import app

    return TestClient(app)


def test_create_list_get_delete_conversation() -> None:
    with _client() as c:
        # Create
        resp = c.post("/api/conversations", json={"title": "My chat"})
        assert resp.status_code == 200, resp.text
        conv = resp.json()
        conv_id = conv["id"]
        assert conv["title"] == "My chat"

        # List
        resp = c.get("/api/conversations")
        assert resp.status_code == 200
        assert any(c["id"] == conv_id for c in resp.json())

        # Get detail (with messages — empty so far)
        resp = c.get(f"/api/conversations/{conv_id}")
        assert resp.status_code == 200
        detail = resp.json()
        assert detail["id"] == conv_id
        assert detail["messages"] == []

        # Delete
        resp = c.delete(f"/api/conversations/{conv_id}")
        assert resp.status_code == 200

        # Now 404
        resp = c.get(f"/api/conversations/{conv_id}")
        assert resp.status_code == 404


def test_get_missing_conversation_404() -> None:
    with _client() as c:
        resp = c.get("/api/conversations/999999")
        assert resp.status_code == 404


def test_patch_conversation_updates_model_and_title() -> None:
    with _client() as c:
        resp = c.post("/api/conversations", json={})
        assert resp.status_code == 200, resp.text
        conv_id = resp.json()["id"]

        # Patch only the model.
        resp = c.patch(f"/api/conversations/{conv_id}", json={"model": "gpt-4o"})
        assert resp.status_code == 200, resp.text
        assert resp.json()["model"] == "gpt-4o"

        # Title should be unchanged by the model-only patch.
        resp = c.get(f"/api/conversations/{conv_id}")
        assert resp.status_code == 200
        detail = resp.json()
        assert detail["model"] == "gpt-4o"
        assert detail["title"] is None

        # Now patch the title too.
        resp = c.patch(
            f"/api/conversations/{conv_id}", json={"title": "Renamed"}
        )
        assert resp.status_code == 200
        patched = resp.json()
        assert patched["title"] == "Renamed"
        # Model must persist from the earlier patch.
        assert patched["model"] == "gpt-4o"


def test_patch_missing_conversation_404() -> None:
    with _client() as c:
        resp = c.patch("/api/conversations/999999", json={"model": "x"})
        assert resp.status_code == 404


# --- working directory + permissions --------------------------------------


def test_create_with_permissions_and_workdir() -> None:
    with _client() as c:
        resp = c.post(
            "/api/conversations",
            json={
                "title": "gated",
                "working_directory": "/tmp/agent-x",
                "permissions": {"*": "ask", "read_file": "allow"},
            },
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["working_directory"] == "/tmp/agent-x"
        assert body["permissions"] == {"*": "ask", "read_file": "allow"}


def test_create_rejects_invalid_permissions() -> None:
    with _client() as c:
        resp = c.post(
            "/api/conversations",
            json={"permissions": {"read_file": "maybe"}},
        )
        assert resp.status_code == 400
        assert "allow|ask|deny" in resp.text


def test_patch_permissions_and_workdir() -> None:
    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]
        resp = c.patch(
            f"/api/conversations/{cid}",
            json={
                "working_directory": "/tmp/agent-y",
                "permissions": {"python_execute": "deny"},
            },
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["working_directory"] == "/tmp/agent-y"
        assert body["permissions"] == {"python_execute": "deny"}


def test_patch_rejects_invalid_permissions() -> None:
    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]
        resp = c.patch(
            f"/api/conversations/{cid}",
            json={"permissions": {"x": "nope"}},
        )
        assert resp.status_code == 400


def test_patch_can_clear_permissions() -> None:
    with _client() as c:
        cid = c.post(
            "/api/conversations", json={"permissions": {"*": "ask"}}
        ).json()["id"]
        resp = c.patch(f"/api/conversations/{cid}", json={"permissions": {}})
        assert resp.status_code == 200
        assert resp.json()["permissions"] is None


# --- approval endpoint ----------------------------------------------------


def test_approval_resolves_pending_request() -> None:
    """Register a pending approval, resolve it via the endpoint."""
    import asyncio

    from app.agent.approvals import approval_registry

    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]

    # Simulate the executor having registered a pending approval.
    async def _setup() -> None:
        approval_registry.register("call_xyz", conversation_id=cid)

    asyncio.run(_setup())

    with _client() as c:
        resp = c.post(
            f"/api/conversations/{cid}/tool_calls/call_xyz/approval",
            json={"approved": True},
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["resolved"] is True
        assert body["approved"] is True

        # A second resolve finds nothing pending → 404.
        resp = c.post(
            f"/api/conversations/{cid}/tool_calls/call_xyz/approval",
            json={"approved": True},
        )
        assert resp.status_code == 404


def test_approval_unknown_conversation_404() -> None:
    with _client() as c:
        resp = c.post(
            "/api/conversations/999999/tool_calls/whatever/approval",
            json={"approved": True},
        )
        assert resp.status_code == 404


def test_approval_deny() -> None:
    """Denying resolves the Future to False."""
    import asyncio

    from app.agent.approvals import approval_registry

    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]

    future_holder: dict = {}

    async def _setup() -> None:
        future_holder["f"] = approval_registry.register("call_deny", conversation_id=cid)

    asyncio.run(_setup())

    with _client() as c:
        resp = c.post(
            f"/api/conversations/{cid}/tool_calls/call_deny/approval",
            json={"approved": False},
        )
        assert resp.status_code == 200
        assert resp.json()["approved"] is False

    # The Future the executor would be awaiting resolves to False.
    assert future_holder["f"].result() is False
