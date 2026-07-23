"""API routes — health, settings, and the simple chat smoke endpoint (MVP).

The full agent loop (with tool-calling, persistence, streaming) lives in
app/agent and will be exposed via /api/conversations/* + /ws/chat/* later
in Фаза 1. For now /api/chat gives a non-streaming smoke test against the
configured provider.
"""

from __future__ import annotations

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

from app.agent import get_default_system_prompt
from app.api.schemas import (
    ChatRequest,
    ChatResponse,
    HealthResponse,
    UsageOut,
)
from app.core.config import get_settings
from app.core.logging import get_logger
from app.providers import Message, get_default_provider

log = get_logger(__name__)

router = APIRouter()


@router.get("/health", response_model=HealthResponse)
async def health() -> HealthResponse:
    settings = get_settings()
    return HealthResponse(
        status="ok",
        version=settings.app_version,
        environment=settings.environment,
    )


@router.post("/chat", response_model=ChatResponse)
async def chat(req: ChatRequest) -> ChatResponse:
    """Non-streaming chat smoke endpoint. No tool-calling, no persistence yet."""
    provider = get_default_provider()
    settings = get_settings()

    messages = [Message(role=m.role, content=m.content) for m in req.messages]
    try:
        result = await provider.chat_completion(
            messages,
            model=req.model or settings.default_model,
            temperature=req.temperature,
            max_tokens=req.max_tokens,
        )
    except Exception as exc:
        log.error("api.chat.failed", error=str(exc))
        raise HTTPException(status_code=502, detail=f"Provider error: {exc}") from exc

    usage = None
    if result.usage:
        usage = UsageOut(
            prompt_tokens=result.usage.prompt_tokens,
            completion_tokens=result.usage.completion_tokens,
            total_tokens=result.usage.total_tokens,
        )

    return ChatResponse(
        content=result.content,
        model=req.model or settings.default_model,
        usage=usage,
        finish_reason=result.finish_reason,
    )


# --- System prompt settings ---


class SystemPromptOut(BaseModel):
    """Current effective system prompt and whether it's customized."""
    prompt: str
    is_custom: bool
    source: str  # "inline" | "file" | "builtin"


class SystemPromptUpdate(BaseModel):
    """Update the system prompt. Empty string resets to built-in default."""
    prompt: str


@router.get("/settings/system-prompt", response_model=SystemPromptOut)
def get_system_prompt() -> SystemPromptOut:
    """Return the current effective system prompt."""
    settings = get_settings()
    prompt = get_default_system_prompt()
    if settings.default_system_prompt:
        return SystemPromptOut(prompt=prompt, is_custom=True, source="inline")
    if settings.system_prompt_file and settings.system_prompt_file.exists():
        return SystemPromptOut(prompt=prompt, is_custom=True, source="file")
    return SystemPromptOut(prompt=prompt, is_custom=False, source="builtin")


@router.put("/settings/system-prompt", response_model=SystemPromptOut)
def update_system_prompt(body: SystemPromptUpdate) -> SystemPromptOut:
    """Update the default system prompt (stored in settings, applied to all new runs).

    Pass an empty string to reset to the built-in default.
    Note: This updates the in-memory settings for the current process. For
    persistence across restarts, set DEFAULT_SYSTEM_PROMPT in .env.
    """
    settings = get_settings()
    # Update the cached settings instance (lru_cache singleton).
    settings.default_system_prompt = body.prompt
    prompt = get_default_system_prompt()
    is_custom = bool(body.prompt.strip())
    return SystemPromptOut(
        prompt=prompt,
        is_custom=is_custom,
        source="inline" if is_custom else "builtin",
    )
