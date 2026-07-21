"""Provider registry.

Resolves which concrete provider + credentials to use for a given request.
For MVP it falls back to settings (env-based) credentials; later it will look
up the `Provider` table row chosen by the user (see Фаза 1 / providers API).
"""

from __future__ import annotations

from functools import lru_cache

from app.core.config import get_settings
from app.core.logging import get_logger
from app.providers.base import LLMProvider
from app.providers.openai import OpenAIProvider

log = get_logger(__name__)


@lru_cache
def get_default_provider() -> LLMProvider:
    """Return the provider configured by settings (MVP).

    Currently builds an OpenAI-compatible provider that also covers OpenRouter,
    DeepSeek, Groq, and Ollama via the `openai_base_url` override.
    Anthropic/subscription providers will be wired here in Фаза 1.
    """
    settings = get_settings()

    if settings.default_provider in {"openai", "openrouter", "deepseek", "groq", "ollama", "local"}:
        if not settings.openai_api_key and settings.default_provider != "ollama":
            log.warning(
                "providers.no_api_key",
                provider=settings.default_provider,
                hint="Set OPENAI_API_KEY (or your provider's key) in .env",
            )
        return OpenAIProvider(
            base_url=settings.openai_base_url,
            api_key=settings.openai_api_key or "ollama",  # ollama ignores the key
            default_model=settings.default_model,
        )

    # Anthropic / subscription adapters land here later.
    raise ValueError(f"Unknown default_provider: {settings.default_provider!r}")
