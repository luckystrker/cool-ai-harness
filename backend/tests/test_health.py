"""Smoke tests for the MVP: app import, routes, health endpoint."""

from __future__ import annotations

from fastapi.testclient import TestClient


def test_app_imports() -> None:
    """The FastAPI app should construct without errors."""
    from app.main import app

    assert app is not None
    assert app.title == "Cool AI Harness"


def test_health_endpoint() -> None:
    """/api/health should return 200 with status=ok and version info."""
    from app.main import app

    with TestClient(app) as client:
        resp = client.get("/api/health")
    assert resp.status_code == 200
    body = resp.json()
    assert body["status"] == "ok"
    assert "version" in body
    assert "environment" in body


def test_chat_route_registered() -> None:
    """The /api/chat and /api/health routes should respond."""
    from app.main import app

    with TestClient(app) as client:
        # /api/health should return 200.
        resp = client.get("/api/health")
        assert resp.status_code == 200

        # /api/chat requires a POST body; a bare GET should 405 (method not
        # allowed), which still proves the route is registered.
        resp = client.get("/api/chat")
        assert resp.status_code in (200, 405), f"Unexpected status: {resp.status_code}"


def test_deliberate_failure() -> None:
    """This test deliberately fails to verify CI blocks merge."""
    assert False, "Deliberate failure to test branch protection"
