"""Conversation and Message models."""

from __future__ import annotations

from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin


class Conversation(TimestampMixin, table=True):
    __tablename__ = "conversations"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    title: str | None = None
    # Which provider/model combination was used for this conversation.
    provider: str | None = None
    model: str | None = None
    # Per-conversation working directory for file/code tools. None = use the
    # global default (settings.default_working_directory or workspaces_dir).
    working_directory: str | None = None
    # Per-conversation tool permissions, overriding the global defaults.
    # Shape: {"*": "ask", "read_file": "allow", "python_execute": "deny"}
    # (tool name -> "allow" | "ask" | "deny"). See app/agent/permissions.py.
    permissions: dict[str, Any] | None = Field(
        default=None, sa_column=Column("permissions", JSON)
    )
    # Free-form metadata (e.g. agent_profile_id once Фаза 3a lands).
    metadata_: dict[str, Any] | None = Field(
        default=None, sa_column=Column("metadata_", JSON)
    )


class Message(TimestampMixin, table=True):
    __tablename__ = "messages"

    id: int | None = Field(default=None, primary_key=True)
    conversation_id: int = Field(foreign_key="conversations.id", index=True)
    role: str  # user | assistant | system | tool
    content: str | None = Field(default=None, sa_column=Column(Text))
    # Tool calls requested by the assistant (list of {id, name, arguments}).
    tool_calls: list[dict[str, Any]] | None = Field(
        default=None, sa_column=Column(JSON)
    )
    # Tool result payload (for role="tool").
    tool_result: dict[str, Any] | None = Field(default=None, sa_column=Column(JSON))
    # Token usage recorded for assistant messages (prompt/completion/total + cost).
    usage: dict[str, Any] | None = Field(default=None, sa_column=Column(JSON))
    # Reasoning / chain-of-thought produced by the model (assistant messages),
    # when the provider exposes one. Kept so reloaded history can show the
    # thinking block that led to an answer.
    thinking: str | None = Field(default=None, sa_column=Column(Text))


class ToolCall(TimestampMixin, table=True):
    """Observability record: one row per tool invocation (see Фаза 3a)."""

    __tablename__ = "tool_calls"

    id: int | None = Field(default=None, primary_key=True)
    conversation_id: int | None = Field(default=None, foreign_key="conversations.id", index=True)
    message_id: int | None = Field(default=None, foreign_key="messages.id", index=True)
    user_id: int | None = Field(default=None, foreign_key="users.id", index=True)
    name: str
    arguments: dict[str, Any] | None = Field(default=None, sa_column=Column(JSON))
    result: Any | None = Field(default=None, sa_column=Column(JSON))
    duration_ms: int | None = None
    success: bool = True
    error: str | None = Field(default=None, sa_column=Column(Text))
