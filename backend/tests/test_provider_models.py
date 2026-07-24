"""Tests for model discovery: list_models(), pricing accessor, and the
/providers/models/* endpoints.

Network is avoided two ways:
  * Pure helpers (``_extract_context_window``, ``get_model_pricing``) are tested
    directly.
  * The HTTP-touching endpoints are exercised with a fake provider whose
    ``list_models`` returns canned data, monkeypatched onto the API module's
    ``build_provider_from_*`` factories.
"""

from __future__ import annotations

from typing import Any

from fastapi.testclient import TestClient


def _client() -> TestClient:
    from app.main import app

    return TestClient(app)


# --- pure helpers -----------------------------------------------------------


def test_extract_context_window_reads_common_field_names() -> None:
    from app.providers.openai import _extract_context_window

    assert _extract_context_window({"context_length": 128000}) == 128000
    assert _extract_context_window({"context_window": 8192}) == 8192
    # LM Studio nests it under top_provider.
    assert _extract_context_window({"top_provider": {"context_length": 32768}}) == 32768
    # Absent / junk -> None.
    assert _extract_context_window({"id": "x"}) is None
    assert _extract_context_window({"context_length": "nope"}) is None
    assert _extract_context_window({"context_length": 0}) is None


def test_get_model_pricing_matches_by_prefix() -> None:
    from app.providers.pricing import get_model_pricing

    # Exact key.
    assert get_model_pricing("gpt-4o-mini") == {"prompt": 0.00015, "completion": 0.0006}
    # Dated variant normalizes to the prefix entry.
    assert get_model_pricing("gpt-4o-2024-08-06")["prompt"] == 0.0025
    # Unknown -> None.
    assert get_model_pricing("totally-made-up-model") is None


# --- endpoints (provider fakes replace real HTTP) ---------------------------


class _FakeProvider:
    """Minimal stand-in: only list_models() is exercised by these endpoints."""

    def __init__(self, models: list[dict[str, Any]], error: Exception | None = None) -> None:
        self._models = models
        self._error = error

    async def list_models(self):
        if self._error is not None:
            raise self._error
        from app.providers.base import ModelInfo

        return [
            ModelInfo(
                id=m["id"],
                context_window=m.get("context_window"),
                prompt_price=m.get("prompt_price"),
                completion_price=m.get("completion_price"),
            )
            for m in self._models
        ]


def test_preview_models_endpoint_returns_annotated_list(monkeypatch) -> None:
    fake = _FakeProvider(
        [
            {"id": "gpt-4o-mini", "context_window": 128000,
             "prompt_price": 0.00015, "completion_price": 0.0006},
            {"id": "gpt-4o", "context_window": 128000},
        ]
    )
    monkeypatch.setattr(
        "app.api.providers.build_provider_from_form",
        lambda **kw: fake,
    )

    with _client() as c:
        resp = c.post(
            "/api/providers/models/preview",
            json={
                "name": "openai",
                "base_url": "https://api.openai.com/v1",
                "api_key": "sk-test",
            },
        )
    assert resp.status_code == 200, resp.text
    data = resp.json()
    # Endpoint preserves provider order (the real provider sorts; the fake
    # here returns them as given).
    assert [m["id"] for m in data] == ["gpt-4o-mini", "gpt-4o"]
    mini = next(m for m in data if m["id"] == "gpt-4o-mini")
    assert mini["context_window"] == 128000
    assert mini["prompt_price"] == 0.00015


def test_preview_models_endpoint_surfaces_provider_error_as_502(monkeypatch) -> None:
    fake = _FakeProvider([], error=RuntimeError("boom"))
    monkeypatch.setattr(
        "app.api.providers.build_provider_from_form",
        lambda **kw: fake,
    )

    with _client() as c:
        resp = c.post(
            "/api/providers/models/preview",
            json={"name": "openai", "api_key": "sk-test"},
        )
    assert resp.status_code == 502
    assert "boom" in resp.json()["detail"]


def test_list_provider_models_endpoint_for_saved_row(monkeypatch) -> None:
    fake = _FakeProvider([{"id": "claude-3-5-sonnet-latest"}])
    monkeypatch.setattr(
        "app.api.providers.build_provider_from_row",
        lambda row: fake,
    )

    with _client() as c:
        # Create a provider row first (the endpoint needs a real id).
        pid = c.post(
            "/api/providers",
            json={"name": "anthropic", "api_key": "sk-ant-test1234"},
        ).json()["id"]
        resp = c.get(f"/api/providers/{pid}/models")

    assert resp.status_code == 200, resp.text
    assert resp.json() == [
        {
            "id": "claude-3-5-sonnet-latest",
            "context_window": None,
            "prompt_price": None,
            "completion_price": None,
        }
    ]


def test_list_provider_models_404_for_unknown_id() -> None:
    with _client() as c:
        assert c.get("/api/providers/999999/models").status_code == 404


# --- OpenAIProvider.list_models() against a mocked /models response ---------


def test_openai_list_models_parses_and_annotates() -> None:
    """list_models() reads /models, extracts context_length, fills prices."""
    import httpx

    from app.providers.openai import OpenAIProvider

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v1/models"
        return httpx.Response(
            200,
            json={
                "data": [
                    {"id": "gpt-4o-mini", "context_length": 128000},
                    {"id": "gpt-4o-2024-08-06", "context_length": 128000},
                    {"id": "text-embedding-3-small"},  # no pricing entry
                ]
            },
        )

    p = OpenAIProvider(
        base_url="https://api.openai.com/v1",
        api_key="sk-test",
        transport=httpx.MockTransport(handler),
    )
    import asyncio

    models = asyncio.run(p.list_models())
    by_id = {m.id: m for m in models}
    assert set(by_id) == {"gpt-4o-2024-08-06", "gpt-4o-mini", "text-embedding-3-small"}
    # Dated variant resolves to the gpt-4o pricing entry via prefix match.
    assert by_id["gpt-4o-2024-08-06"].prompt_price == 0.0025
    # context_length carried through.
    assert by_id["gpt-4o-mini"].context_window == 128000
    # No pricing entry -> None (not an error).
    assert by_id["text-embedding-3-small"].prompt_price is None
    assert by_id["text-embedding-3-small"].context_window is None


def test_anthropic_list_models_uses_v1_models_endpoint() -> None:
    """Anthropic lists via /v1/models with x-api-key; context stays None."""
    import httpx

    from app.providers.anthropic import AnthropicProvider

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v1/models"
        assert request.headers["x-api-key"] == "sk-ant-test"
        return httpx.Response(
            200,
            json={"data": [{"id": "claude-3-5-sonnet-latest"}]},
        )

    p = AnthropicProvider(
        base_url="https://api.anthropic.com",
        api_key="sk-ant-test",
        transport=httpx.MockTransport(handler),
    )
    import asyncio

    models = asyncio.run(p.list_models())
    assert len(models) == 1
    assert models[0].id == "claude-3-5-sonnet-latest"
    # Anthropic /models doesn't return a context length.
    assert models[0].context_window is None
    # Price comes from the local table.
    assert models[0].prompt_price == 0.003

