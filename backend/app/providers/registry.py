"""Provider registry.

Resolves which concrete provider + credentials to use for a given request.

Resolution order:
  1. If an ``override`` provider is passed explicitly (e.g. selected by name
     from the UI / model picker) — use it.
  2. Otherwise look for an **active Provider row in the database** (the one
     the user configured via /api/providers). The first active row wins;
     later we can store a "is_default" flag or pick by provider name.
  3. Fall back to settings (env-based) credentials — useful for dev and tests.

The database lookup is optional (works without a session in pure unit tests);
``get_default_provider()`` keeps the env-only fast path for tests.
"""

from __future__ import annotations

from sqlmodel import Session, select

from app.core.config import get_settings
from app.core.db import engine
from app.core.logging import get_logger
from app.core.security import decrypt
from app.models import Provider as ProviderRow
from app.providers.anthropic import AnthropicProvider
from app.providers.base import LLMProvider
from app.providers.openai import OpenAIProvider
from app.providers.resilience import ResilientProvider

log = get_logger(__name__)


def _provider_row_to_llm(row: ProviderRow) -> LLMProvider:
    """Build a concrete LLMProvider from a stored Provider row."""
    # Decrypt the stored key. If it fails, fall back to env so the agent still
    # runs (with a loud warning) rather than crashing the whole turn.
    api_key = ""
    if row.api_key_encrypted:
        try:
            api_key = decrypt(row.api_key_encrypted)
        except ValueError:
            log.error("providers.decrypt_failed", provider_id=row.id, name=row.name)

    # Resolve base_url: explicit row value > provider-class default > settings.
    base_url = row.base_url or _default_base_url_for(row.name)
    model = row.default_model or get_settings().default_model

    name = (row.name or "").lower()
    if name == "anthropic":
        # Native Anthropic Messages API — not OpenAI-compatible.
        return AnthropicProvider(
            base_url=base_url,
            api_key=api_key,
            default_model=model,
        )

    # Every OpenAI-compatible backend (openai/openrouter/deepseek/groq/ollama/
    # local) is served by OpenAIProvider. Subscription adapters will dispatch
    # to their own classes here once added.
    return OpenAIProvider(
        base_url=base_url,
        api_key=api_key or "ollama",  # ollama ignores the key
        default_model=model,
    )


def _default_base_url_for(name: str) -> str:
    defaults = {
        "openai": "https://api.openai.com/v1",
        "anthropic": "https://api.anthropic.com",
        "openrouter": "https://openrouter.ai/api/v1",
        "open_router": "https://openrouter.ai/api/v1",
        "deepseek": "https://api.deepseek.com/v1",
        "groq": "https://api.groq.com/openai/v1",
        "ollama": "http://localhost:11434/v1",
        "local": "http://localhost:11434/v1",
    }
    return defaults.get(name.lower(), get_settings().openai_base_url)


def get_provider_from_db(session: Session) -> LLMProvider | None:
    """Return the resilient provider chain built from active Provider rows.

    Primary = first active, non-fallback row. Fallbacks = active rows marked
    ``is_fallback`` (in id order). The chain is wrapped in a
    ``ResilientProvider`` (retry / circuit breaker / fallback, Фаза 1.5 §5).
    Returns None when no active primary row exists.
    """
    rows = list(
        session.exec(
            select(ProviderRow)
            .where(ProviderRow.user_id == 1)
            .where(ProviderRow.is_active == True)  # noqa: E712
            .order_by(ProviderRow.id)
        ).all()
    )
    if not rows:
        return None
    primary_rows = [r for r in rows if not r.is_fallback]
    fallback_rows = [r for r in rows if r.is_fallback]
    # If nothing is explicitly non-fallback, treat the first row as primary.
    if not primary_rows:
        primary_rows = [fallback_rows[0]]
        fallback_rows = fallback_rows[1:]

    primary = _provider_row_to_llm(primary_rows[0])
    log.debug("providers.selected_from_db", id=primary_rows[0].id, name=primary_rows[0].name)

    if not fallback_rows:
        # Still wrap for retry/backoff/circuit-breaker even without a fallback.
        return ResilientProvider(primary)

    fallbacks = [_provider_row_to_llm(r) for r in fallback_rows]
    for r in fallback_rows:
        log.debug("providers.fallback", id=r.id, name=r.name)
    return ResilientProvider(primary, fallbacks=fallbacks)


def _wrap_resilient(provider: LLMProvider) -> LLMProvider:
    """Wrap a settings-only provider with retry/circuit-breaker (no fallback)."""
    if isinstance(provider, ResilientProvider):
        return provider
    return ResilientProvider(provider)


def get_default_provider() -> LLMProvider:
    """Return the provider to use for a turn.

    Tries the database first (user-configured provider via /api/providers),
    then falls back to environment-based settings. The DB lookup opens a
    short-lived session; on any error it falls back to settings too.
    """
    try:
        with Session(engine) as session:
            from_db = get_provider_from_db(session)
        if from_db is not None:
            return from_db
    except Exception as exc:
        log.warning("providers.db_lookup_failed", error=str(exc))

    return _wrap_resilient(_from_settings())


def _from_settings() -> LLMProvider:
    """Env-only provider — the pre-database behavior (tests, dev, no UI setup)."""
    settings = get_settings()
    if settings.default_provider == "anthropic":
        if not settings.anthropic_api_key:
            log.warning(
                "providers.no_api_key",
                provider="anthropic",
                hint="Set ANTHROPIC_API_KEY in .env",
            )
        return AnthropicProvider(
            base_url=settings.anthropic_base_url,
            api_key=settings.anthropic_api_key,
            default_model=settings.default_model,
        )

    if settings.default_provider in {
        "openai", "openrouter", "deepseek", "groq", "ollama", "local", "open_router",
    }:
        if not settings.openai_api_key and settings.default_provider != "ollama":
            log.warning(
                "providers.no_api_key",
                provider=settings.default_provider,
                hint="Set OPENAI_API_KEY (or your provider's key) in .env",
            )
        return OpenAIProvider(
            base_url=settings.openai_base_url,
            api_key=settings.openai_api_key or "ollama",
            default_model=settings.default_model,
        )

    raise ValueError(f"Unknown default_provider: {settings.default_provider!r}")


# Keep the old import path working for callers that imported it by that name.
def get_default_provider_cached() -> LLMProvider:
    """Uncached alias. (Kept for compatibility; the old lru_cache was removed
    because provider rows can change at runtime via the UI.)"""
    return get_default_provider()
