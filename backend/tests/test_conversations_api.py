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
