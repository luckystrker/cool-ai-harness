"""Pydantic request/response schemas for the HTTP API."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field


class HealthResponse(BaseModel):
    status: str = "ok"
    version: str
    environment: str


class ChatMessageIn(BaseModel):
    """One incoming message for the simple /api/chat smoke endpoint (MVP)."""

    role: str = Field(default="user")
    content: str


class ChatRequest(BaseModel):
    """Simple non-streaming chat request (MVP smoke test).

    The full agent loop with tool-calling + persistence lands later in Фаза 1.
    """

    messages: list[ChatMessageIn]
    model: str | None = None
    temperature: float = 0.7
    max_tokens: int | None = None


class UsageOut(BaseModel):
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0


class ChatResponse(BaseModel):
    content: str | None
    model: str
    usage: UsageOut | None = None
    finish_reason: str | None = None
    raw: Any | None = None
