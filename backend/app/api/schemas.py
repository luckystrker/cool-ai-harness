"""Pydantic request/response schemas for the HTTP API."""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field

# --- health ---


class HealthResponse(BaseModel):
    status: str = "ok"
    version: str
    environment: str


# --- chat (MVP smoke endpoint) ---


class ChatMessageIn(BaseModel):
    role: str = Field(default="user")
    content: str


class ChatRequest(BaseModel):
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


# --- conversations ---


class ConversationCreate(BaseModel):
    title: str | None = None
    system_prompt: str | None = None
    model: str | None = None
    tool_names: list[str] | None = None


class ConversationOut(BaseModel):
    id: int
    user_id: int
    title: str | None = None
    model: str | None = None
    created_at: datetime
    updated_at: datetime


class MessageOut(BaseModel):
    id: int
    conversation_id: int
    role: str
    content: str | None = None
    tool_calls: list[dict[str, Any]] | None = None
    usage: dict[str, Any] | None = None
    created_at: datetime


class ConversationDetail(ConversationOut):
    messages: list[MessageOut] = []


class SendMessageRequest(BaseModel):
    content: str
    # Optional per-message overrides.
    model: str | None = None
    system_prompt: str | None = None
    tool_names: list[str] | None = None
