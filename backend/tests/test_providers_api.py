"""Tests for the providers API: encrypted credential CRUD."""

from __future__ import annotations

from fastapi.testclient import TestClient


def _client() -> TestClient:
    from app.main import app

    return TestClient(app)


def test_create_list_update_delete_provider() -> None:
    with _client() as c:
        # Create
        resp = c.post(
            "/api/providers",
            json={
                "name": "openai",
                "label": "Personal",
                "base_url": "https://api.openai.com/v1",
                "api_key": "sk-supersecretkey1234",
                "default_model": "gpt-4o-mini",
            },
        )
        assert resp.status_code == 200, resp.text
        p = resp.json()
        pid = p["id"]
        # Key is masked — never the full secret.
        assert p["api_key_hint"].startswith("sk-")
        assert "supersecretkey1234" not in p["api_key_hint"]
        assert "…" in p["api_key_hint"]

        # List
        resp = c.get("/api/providers")
        assert resp.status_code == 200
        assert any(x["id"] == pid for x in resp.json())

        # Get detail (also masked)
        resp = c.get(f"/api/providers/{pid}")
        assert resp.status_code == 200
        assert resp.json()["api_key_hint"] == p["api_key_hint"]

        # Update (rotate key + change label)
        resp = c.patch(
            f"/api/providers/{pid}",
            json={"api_key": "sk-rotatednewkey5678", "label": "Rotated"},
        )
        assert resp.status_code == 200
        updated = resp.json()
        assert updated["label"] == "Rotated"
        assert updated["api_key_hint"] != p["api_key_hint"]

        # The encrypted blob stored in the DB must differ from the plaintext.
        from sqlmodel import Session

        from app.core.db import engine
        from app.models import Provider as ProviderRow

        with Session(engine) as s:
            row = s.get(ProviderRow, pid)
            assert row is not None
            assert "sk-rotatednewkey5678" not in (row.api_key_encrypted or "")
            # And it should decrypt back to the rotated key.
            from app.core.security import decrypt

            assert decrypt(row.api_key_encrypted) == "sk-rotatednewkey5678"

        # Delete
        resp = c.delete(f"/api/providers/{pid}")
        assert resp.status_code == 200
        resp = c.get(f"/api/providers/{pid}")
        assert resp.status_code == 404


def test_get_missing_provider_404() -> None:
    with _client() as c:
        assert c.get("/api/providers/999999").status_code == 404
