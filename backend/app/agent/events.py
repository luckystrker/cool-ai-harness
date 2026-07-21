"""Agent loop events.

The AgentExecutor yields a stream of AgentEvent objects. They are the single
source of truth that the API layer (SSE / WebSocket / Telegram) translates
into whatever wire format the transport needs. Keeping them transport-agnostic
means the same loop drives chat, subagents, and cron jobs (Фаза 3b).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal

from app.providers import Usage

# Event type tags. Keep stable — clients (frontend, telegram) parse them.
EventKind = Literal[
    "start",                # loop started
    "token",                # a streamed assistant text token
    "tool_call_start",      # model requested a tool call (full call info)
    "tool_call_delta",      # incremental tool-call args fragment (rare; usually we batch)
    "tool_result",          # tool finished with its ToolResult
    "message",              # a complete assistant message persisted
    "finish",               # loop finished (terminal); carries usage + reason
    "error",                # unrecoverable error
]


@dataclass
class AgentEvent:
    kind: EventKind
    # Free-form payload, shape depends on kind:
    #   token:            {"text": str}
    #   tool_call_start:  {"id": str, "name": str, "arguments": dict}
    #   tool_result:      {"id": str, "name": str, "result": ToolResult-dict}
    #   message:          {"role": "assistant", "content": str, "tool_calls": ...}
    #   finish:           {"reason": str, "usage": Usage-dict | None, "iterations": int}
    #   error:            {"message": str, "detail": str | None}
    payload: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {"kind": self.kind, "payload": self.payload}

    def to_dict_json(self) -> str:
        """JSON-serialized to_dict(), defaulting unknown types to str."""
        import json

        return json.dumps(self.to_dict(), default=str, ensure_ascii=False)

    # --- ergonomic constructors ---

    @classmethod
    def start(cls, *, conversation_id: int | None = None) -> AgentEvent:
        return cls(kind="start", payload={"conversation_id": conversation_id})

    @classmethod
    def token(cls, text: str) -> AgentEvent:
        return cls(kind="token", payload={"text": text})

    @classmethod
    def tool_call_start(cls, *, call_id: str, name: str, arguments: dict[str, Any]) -> AgentEvent:
        return cls(
            kind="tool_call_start",
            payload={"id": call_id, "name": name, "arguments": arguments},
        )

    @classmethod
    def tool_result(cls, *, call_id: str, name: str, result: Any) -> AgentEvent:
        return cls(
            kind="tool_result",
            payload={"id": call_id, "name": name, "result": result},
        )

    @classmethod
    def message(cls, *, content: str | None, tool_calls: list[dict] | None) -> AgentEvent:
        return cls(
            kind="message",
            payload={"role": "assistant", "content": content, "tool_calls": tool_calls},
        )

    @classmethod
    def finish(cls, *, reason: str, usage: Usage | None, iterations: int) -> AgentEvent:
        return cls(
            kind="finish",
            payload={
                "reason": reason,
                "usage": vars(usage) if usage else None,
                "iterations": iterations,
            },
        )

    @classmethod
    def error(cls, message: str, detail: str | None = None) -> AgentEvent:
        return cls(kind="error", payload={"message": message, "detail": detail})
