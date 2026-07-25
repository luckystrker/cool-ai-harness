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
    """The /api/chat smoke route should be registered."""
    from app.main import app

    def _collect_paths(routes: list) -> set[str]:
        paths: set[str] = set()
        for r in routes:
            if hasattr(r, "path"):
                paths.add(r.path)
            if hasattr(r, "routes"):
                paths |= _collect_paths(r.routes)
        return paths

    routes = _collect_paths(app.routes)
    assert "/api/chat" in routes
    assert "/api/health" in routes
