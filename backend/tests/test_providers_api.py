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


def test_chat_models_roundtrip_and_default() -> None:
    """chat_models are persisted, returned as a list, and editable."""
    with _client() as c:
        resp = c.post(
            "/api/providers",
            json={
                "name": "openai",
                "api_key": "sk-supersecretkey1234",
                "chat_models": ["gpt-4o", "gpt-4o-mini"],
            },
        )
        assert resp.status_code == 200, resp.text
        p = resp.json()
        pid = p["id"]
        assert p["chat_models"] == ["gpt-4o", "gpt-4o-mini"]

        # GET returns the same list.
        assert c.get(f"/api/providers/{pid}").json()["chat_models"] == ["gpt-4o", "gpt-4o-mini"]

        # PATCH replaces the list wholesale.
        updated = c.patch(
            f"/api/providers/{pid}",
            json={"chat_models": ["gpt-4o"]},
        ).json()
        assert updated["chat_models"] == ["gpt-4o"]

        # Omitting chat_models on PATCH leaves it untouched.
        again = c.patch(f"/api/providers/{pid}", json={"label": "x"}).json()
        assert again["chat_models"] == ["gpt-4o"]


def test_provider_default_without_chat_models() -> None:
    """Providers created without chat_models get an empty list on read."""
    with _client() as c:
        pid = c.post(
            "/api/providers", json={"name": "openai", "api_key": "sk-1234567890abc"}
        ).json()["id"]
        assert c.get(f"/api/providers/{pid}").json()["chat_models"] == []


def test_registry_default_model_prefers_chat_models_first() -> None:
    """build_provider_from_row uses chat_models[0] as the default model."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.models import Provider as ProviderRow
    from app.providers.registry import build_provider_from_row

    with _client() as c:
        pid = c.post(
            "/api/providers",
            json={
                "name": "openai",
                "api_key": "sk-supersecretkey1234",
                "chat_models": ["gpt-4o-mini", "gpt-4o"],
            },
        ).json()["id"]

    with Session(engine) as s:
        row = s.get(ProviderRow, pid)
        assert row is not None
        provider = build_provider_from_row(row)
        # The first chat-exposed model is the effective default.
        assert provider.default_model == "gpt-4o-mini"


def test_is_default_is_mutually_exclusive() -> None:
    """Marking a provider default clears the flag on all others."""
    with _client() as c:
        a = c.post(
            "/api/providers", json={"name": "openai", "api_key": "sk-aaaakey1234"}
        ).json()
        b = c.post(
            "/api/providers", json={"name": "anthropic", "api_key": "sk-bbbbkey1234"}
        ).json()

        # Mark A as default.
        c.patch(f"/api/providers/{a['id']}", json={"is_default": True})
        assert c.get(f"/api/providers/{a['id']}").json()["is_default"] is True
        assert c.get(f"/api/providers/{b['id']}").json()["is_default"] is False

        # Mark B as default -> A loses the flag.
        c.patch(f"/api/providers/{b['id']}", json={"is_default": True})
        assert c.get(f"/api/providers/{b['id']}").json()["is_default"] is True
        assert c.get(f"/api/providers/{a['id']}").json()["is_default"] is False


def test_new_conversation_seeds_model_from_default_provider() -> None:
    """A new conversation gets the default provider's first chat model when the
    caller doesn't name one."""
    with _client() as c:
        # Default provider with two chat models.
        c.post(
            "/api/providers",
            json={
                "name": "openai",
                "api_key": "sk-supersecretkey1234",
                "chat_models": ["gpt-4o-mini", "gpt-4o"],
                "is_default": True,
            },
        )
        conv = c.post("/api/conversations", json={}).json()
        assert conv["model"] == "gpt-4o-mini"

        # An explicit model is respected.
        conv2 = c.post("/api/conversations", json={"model": "gpt-4o"}).json()
        assert conv2["model"] == "gpt-4o"
