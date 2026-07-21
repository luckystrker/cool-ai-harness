"""Provider credential model.

Stores API keys / OAuth tokens for LLM providers. The `api_key_encrypted`
column is encrypted at rest via app.core.security (Fernet).
"""

from __future__ import annotations

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
