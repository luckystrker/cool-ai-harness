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
    # Per-conversation working directory (overrides the global default).
    working_directory: str | None = None
    # Per-conversation tool permissions: {"*": "ask", "read_file": "allow", ...}
    # Each value must be one of allow|ask|deny. Validated by the route layer.
    permissions: dict[str, str] | None = None
    # Per-conversation capability policy: {"execute": "ask", "network": "ask", ...}
    # Each value must be one of allow|ask|deny. Validated by the route layer.
    capability_policy: dict[str, str] | None = None
    # Per-conversation breakpoints: [{"type": "before_write", "tool": "write_file"}, ...]
    # Stored in conversation metadata. See app/security/breakpoints.py.
    breakpoints: list[dict[str, Any]] | None = None


class ConversationUpdate(BaseModel):
    # All fields optional; only provided fields are applied.
    title: str | None = None
    model: str | None = None
    working_directory: str | None = None
    permissions: dict[str, str] | None = None
    capability_policy: dict[str, str] | None = None
    breakpoints: list[dict[str, Any]] | None = None


class ConversationOut(BaseModel):
    id: int
    user_id: int
    title: str | None = None
    model: str | None = None
    working_directory: str | None = None
    permissions: dict[str, str] | None = None
    capability_policy: dict[str, str] | None = None
    breakpoints: list[dict[str, Any]] | None = None
    created_at: datetime
    updated_at: datetime


class MessageOut(BaseModel):
    id: int
    conversation_id: int
    role: str
    content: str | None = None
    tool_calls: list[dict[str, Any]] | None = None
    usage: dict[str, Any] | None = None
    thinking: str | None = None
    tool_result: dict[str, Any] | None = None
    created_at: datetime


class ConversationDetail(ConversationOut):
    messages: list[MessageOut] = []


class SendMessageRequest(BaseModel):
    content: str
    # Optional per-message overrides.
    model: str | None = None
    system_prompt: str | None = None
    tool_names: list[str] | None = None


class ToolApprovalRequest(BaseModel):
    """Client decision for a pending tool-call approval.

    ``call_id`` may also be supplied in the URL path; the body value wins if
    both are present. ``approved=False`` denies the call (the loop continues
    with a Permission-denied tool_result).
    """

    approved: bool


# --- agent runs (Фаза 1.5 — durable runs) ---


class RunEventOut(BaseModel):
    """One row of a run's append-only event log."""

    id: int
    run_id: int
    seq: int
    kind: str
    payload: dict[str, Any] | None = None
    created_at: datetime


class RunOut(BaseModel):
    """Summary of a run (list/detail)."""

    id: int
    conversation_id: int
    status: str
    model: str | None = None
    iterations: int = 0
    usage: dict[str, Any] | None = None
    finish_reason: str | None = None
    error: str | None = None
    started_at: datetime
    finished_at: datetime | None = None
    created_at: datetime
    updated_at: datetime


class RunDetail(RunOut):
    """A run plus its config snapshot, checkpoint, and full event log."""

    config: dict[str, Any] | None = None
    checkpoint: dict[str, Any] | None = None
    events: list[RunEventOut] = []


class CancelRunResponse(BaseModel):
    """Result of a cancel request."""

    run_id: int
    cancelled: bool


# --- approval audit (Фаза 1.5 §2 — approval audit trail) ---


class ApprovalAuditOut(BaseModel):
    """One row of the approval audit trail."""

    id: int
    conversation_id: int
    run_id: int | None = None
    call_id: str
    tool_name: str
    arguments: dict[str, Any] | None = None
    approved: bool
    decision_source: str
    decided_by: str | None = None
    reason: str | None = None
    is_breakpoint: bool = False
    breakpoint_type: str | None = None
    duration_ms: int | None = None
    created_at: datetime
