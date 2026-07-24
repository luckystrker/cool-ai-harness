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
    # Dev defaults cover Vite (5173), and common alt ports.
    cors_origins: list[str] = Field(
        default_factory=lambda: [
            "http://localhost:5173",
            "http://127.0.0.1:5173",
            "http://localhost:4173",
        ]
    )

    # --- Database ---
    # SQLite by default. Use e.g. postgresql+psycopg://user:pass@host/db for product.
    database_url: str = f"sqlite:///{REPO_ROOT / 'data' / 'harness.db'}"

    # --- Security ---
    # Used to encrypt stored API keys at rest (Fernet). Generate with:
    #   python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())"
    secret_key: str = Field(default="CHANGE_ME", description="Fernet key for encrypting secrets")
    # Bearer token for the single-user auth (MVP). Empty disables auth check.
    api_token: str = Field(default="", description="Bearer token for API auth (MVP single-user)")

    # --- Env-only LLM credentials (dev / test fallback) ---
    # The user-configured providers in the database are the primary source at
    # runtime (see app.providers.registry). These env vars are a fallback for
    # local dev and the test suite when no Provider row exists yet. The active
    # backend is picked by which key is set (ANTHROPIC_API_KEY => Anthropic,
    # otherwise OpenAI-compatible at OPENAI_BASE_URL).
    # Default OpenAI-compatible endpoint. Override for OpenRouter/DeepSeek/Groq/Ollama.
    openai_base_url: str = "https://api.openai.com/v1"
    openai_api_key: str = ""
    # Native Anthropic (Claude) Messages API.
    anthropic_base_url: str = "https://api.anthropic.com"
    anthropic_api_key: str = ""

    # --- Telegram ---
    telegram_bot_token: str = ""

    # --- Web search (tools) ---
    search_provider: str = Field(
        default="", description="serper | tavily | searxng (empty = tool errors gracefully)"
    )
    serper_api_key: str = ""
    tavily_api_key: str = ""
    searxng_url: str = ""  # e.g. http://localhost:8080

    # --- Paths ---
    data_dir: Path = REPO_ROOT / "data"
    workspaces_dir: Path = REPO_ROOT / "workspaces"
    skills_dir: Path = REPO_ROOT / "skills"
    artifacts_dir: Path = REPO_ROOT / "data" / "artifacts"

    # --- Artifacts (Фаза 1.5 §3) ---
    # Max upload size per artifact file (bytes). 0 = no limit.
    artifact_max_upload_bytes: int = Field(
        default=50_000_000,
        description="Max artifact upload size in bytes; 0 = no limit",
    )
    # Max extracted text stored per artifact (chars). Longer text is truncated.
    artifact_max_extracted_chars: int = Field(
        default=100_000,
        description="Max chars of extracted text stored per artifact",
    )

    # --- Agent system prompt ---
    # Path to a custom system prompt file. If empty, uses the built-in default.
    system_prompt_file: Path | None = Field(
        default=None,
        description="Path to custom system prompt file; empty = built-in default",
    )
    # Inline system prompt override (takes precedence over file). Stored in
    # localStorage on the frontend and sent per-request.
    default_system_prompt: str = Field(
        default="",
        description="Default system prompt text; empty = load from file/built-in",
    )

    # --- Agent permissions & working directory (defaults) ---
    # Per-conversation settings override these. Empty path = use workspaces_dir.
    # Set via env as a path string, e.g. DEFAULT_WORKING_DIRECTORY=/tmp/agent.
    default_working_directory: Path | None = Field(
        default=None,
        description="Default agent working directory; empty = workspaces_dir",
    )
    # Default tool permissions applied when a conversation has none. Set via
    # env as a JSON object string, e.g.
    #   DEFAULT_TOOL_PERMISSIONS={"*":"ask","read_file":"allow"}
    # pydantic-settings parses JSON for dict[str,str] fields automatically.
    default_tool_permissions: dict[str, str] = Field(
        default_factory=dict,
        description='Tool permission map: {"*":"ask","read_file":"allow",...}',
    )
    # How long the agent waits for a human approve/deny on an "ask" tool before
    # auto-denying. 5 minutes was far too long for an interactive chat — a
    # forgotten prompt shouldn't tie up a turn for that long.
    approval_timeout_s: float = Field(
        default=30.0,
        description="Seconds to wait for a tool approval before auto-deny",
    )

    # --- Agent run limits (Фаза 1.5 — durable runs) ---
    # Defaults applied to every agent run unless the caller overrides them.
    # All can be unset (None) to disable that particular ceiling.
    agent_max_iterations: int = Field(
        default=10,
        description="Max LLM round-trips per run before stopping",
    )
    agent_max_total_tokens: int | None = Field(
        default=None,
        description="Token ceiling per run; None = no limit",
    )
    agent_max_cost_usd: float | None = Field(
        default=None,
        description="Cost (USD) ceiling per run; None = no limit",
    )
    agent_run_timeout_s: float | None = Field(
        default=None,
        description="Wall-clock timeout per run in seconds; None = no timeout",
    )

    # --- Provider resilience (Фаза 1.5 §5) ---
    # Retries apply to retriable failures (HTTP 429 / 5xx, timeouts, network
    # errors) with exponential backoff + full jitter. A circuit breaker trips
    # per provider after ``provider_circuit_failure_threshold`` consecutive
    # failures and resets after ``provider_circuit_reset_s`` seconds.
    provider_max_retries: int = Field(
        default=3,
        description="Per-provider retry attempts on retriable LLM failures",
    )
    provider_retry_base_delay_s: float = Field(
        default=0.5,
        description="Base (seconds) for exponential backoff; full jitter applied",
    )
    provider_retry_max_delay_s: float = Field(
        default=30.0,
        description="Cap (seconds) on a single backoff delay",
    )
    provider_circuit_failure_threshold: int = Field(
        default=5,
        description="Consecutive failures before a provider's circuit opens",
    )
    provider_circuit_reset_s: float = Field(
        default=60.0,
        description="Seconds before an open circuit moves to half-open (probe)",
    )

    # --- Cost budgets (Фаза 1.5 §5) ---
    # Period budgets (USD). None = no budget for that window. At
    # ``budget_alert_threshold_pct`` of the most relevant budget an alert fires;
    # when ``budget_block_on_exceed`` is True, LLM calls are blocked at 100 %
    # unless the user grants an explicit override (per-user row in `budgets`).
    budget_alert_threshold_pct: float = Field(
        default=80.0,
        description="Spend percentage at which a budget alert fires",
    )
    budget_block_on_exceed: bool = Field(
        default=True,
        description="Block new LLM calls once a budget is exceeded (until override)",
    )

    # --- Capability security (Фаза 1.5 §2) ---
    # Capability-level policy: maps capability name to allow|ask|deny.
    # Applied as a coarse-grained gate BEFORE the per-tool permission check.
    # e.g. {"execute": "ask", "network": "ask", "write": "allow"}
    capability_policy: dict[str, str] = Field(
        default_factory=dict,
        description='Capability policy: {"execute":"ask","network":"ask",...}',
    )
    # Comma-separated list of allowed domains for web_fetch / network tools.
    # Empty = allow all (no domain allowlist). When set, fetches to non-listed
    # domains are denied.
    network_allowed_domains: list[str] = Field(
        default_factory=list,
        description="Domain allowlist for network tools; empty = allow all",
    )
    # When True, block requests to private/internal IP ranges (RFC 1918,
    # loopback, link-local) to prevent SSRF.
    ssrf_block_private_ips: bool = Field(
        default=True,
        description="Block requests to private/internal IPs (SSRF protection)",
    )
    # Max response body size for web_fetch (bytes). 0 = no limit.
    network_max_response_bytes: int = Field(
        default=500_000,
        description="Max response body size for web_fetch; 0 = no limit",
    )
    # When True, mask secrets (API keys, tokens, passwords) in tool outputs,
    # log messages, and LLM-visible tool results.
    mask_secrets: bool = Field(
        default=True,
        description="Mask secrets in tool outputs, messages, traces, and logs",
    )
    # When True, strip environment variables that look like secrets before
    # spawning subprocesses for code execution.
    sandbox_strip_env: bool = Field(
        default=True,
        description="Strip secret-looking env vars from subprocess environment",
    )
    # Breakpoint TTL: how long the executor waits for a breakpoint approval
    # before applying the fallback action.
    breakpoint_timeout_s: float = Field(
        default=60.0,
        description="Seconds to wait for a breakpoint approval before fallback",
    )
    # Fallback action when a breakpoint times out: deny or skip.
    breakpoint_fallback: str = Field(
        default="deny",
        description='Breakpoint timeout fallback: "deny" or "skip"',
    )

    def ensure_dirs(self) -> None:
        """Create runtime directories if they don't exist."""
        for path in (self.data_dir, self.workspaces_dir, self.skills_dir, self.artifacts_dir):
            Path(path).mkdir(parents=True, exist_ok=True)


@lru_cache
def get_settings() -> Settings:
    """Return cached settings instance."""
    settings = Settings()
    settings.ensure_dirs()
    return settings
