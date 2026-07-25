"""Provider credential model.

Stores API keys / OAuth tokens for LLM providers. The `api_key_encrypted`
column is encrypted at rest via app.core.security (Fernet).
"""

from __future__ import annotations

from sqlalchemy import Column
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin


class Provider(TimestampMixin, table=True):
    __tablename__ = "providers"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    # Logical provider name: openai | anthropic | openrouter | deepseek | groq | ollama | subscription/claude_pro ...
    name: str
    # Human label, e.g. "OpenAI personal", "Claude Pro sub".
    label: str | None = None
    # Base URL override (for OpenAI-compatible providers).
    base_url: str | None = None
    # Encrypted secret (API key, OAuth token, refresh token). See app.core.security.
    api_key_encrypted: str | None = None
    # Default model to use with this provider when none specified.
    default_model: str | None = None
    is_active: bool = True
    is_subscription: bool = False
    # When True, this provider is used as a fallback when the primary (first
    # active, non-fallback) provider is unhealthy (Фаза 1.5 §5). A single
    # active fallback row is the expected setup.
    is_fallback: bool = Field(default=False, index=True)
    # When True, this is the default provider for new conversations (its first
    # chat-exposed model becomes the default model, and it's the primary in the
    # resilience chain). Mutually exclusive: at most one row per user.
    is_default: bool = Field(default=False, index=True)
    # JSON list of model ids the user has marked as available in the chat
    # model picker (selected from the provider's live /models list). The first
    # entry is used as the effective default when none is named per-conversation.
    chat_models: list[str] | None = Field(
        default=None, sa_column=Column("chat_models", JSON)
    )
