"""Application configuration loaded from environment variables."""

from __future__ import annotations

from functools import lru_cache
from pathlib import Path

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict

# Repo root (backend/..) — used for default paths.
REPO_ROOT = Path(__file__).resolve().parents[3]


class Settings(BaseSettings):
    """Central configuration. Reads from .env and environment variables."""

    model_config = SettingsConfigDict(
        env_file=REPO_ROOT / ".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
        extra="ignore",
    )

    # --- App ---
    app_name: str = "Cool AI Harness"
    app_version: str = "0.1.0"
    environment: str = Field(default="development", description="development|production")
    debug: bool = True
    # Comma-separated list of allowed CORS origins. "*" allows all.
    cors_origins: list[str] = Field(default_factory=lambda: ["*"])

    # --- Database ---
    # SQLite by default. Use e.g. postgresql+psycopg://user:pass@host/db for product.
    database_url: str = f"sqlite:///{REPO_ROOT / 'data' / 'harness.db'}"

    # --- Security ---
    # Used to encrypt stored API keys at rest (Fernet). Generate with:
    #   python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())"
    secret_key: str = Field(default="CHANGE_ME", description="Fernet key for encrypting secrets")
    # Bearer token for the single-user auth (MVP). Empty disables auth check.
    api_token: str = Field(default="", description="Bearer token for API auth (MVP single-user)")

    # --- Default LLM provider (MVP) ---
    default_provider: str = Field(default="openai", description="openai|anthropic|subscription|local")
    default_model: str = Field(default="gpt-4o-mini")
    # Default OpenAI-compatible endpoint. Override for OpenRouter/DeepSeek/Groq/Ollama.
    openai_base_url: str = "https://api.openai.com/v1"
    openai_api_key: str = ""
    anthropic_api_key: str = ""

    # --- Telegram ---
    telegram_bot_token: str = ""

    # --- Paths ---
    data_dir: Path = REPO_ROOT / "data"
    workspaces_dir: Path = REPO_ROOT / "workspaces"
    skills_dir: Path = REPO_ROOT / "skills"

    def ensure_dirs(self) -> None:
        """Create runtime directories if they don't exist."""
        for path in (self.data_dir, self.workspaces_dir, self.skills_dir):
            Path(path).mkdir(parents=True, exist_ok=True)


@lru_cache
def get_settings() -> Settings:
    """Return cached settings instance."""
    settings = Settings()
    settings.ensure_dirs()
    return settings
