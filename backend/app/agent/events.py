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
    "start",  # loop started
    "thinking",  # a streamed reasoning / chain-of-thought fragment
    "token",  # a streamed assistant text token
    "tool_call_start",  # model requested a tool call (full call info)
    "tool_call_delta",  # incremental tool-call args fragment (rare; usually we batch)
    "tool_approval_request",  # tool needs human approval before running; client must respond
    "tool_result",  # tool finished with its ToolResult
    "message",  # a complete assistant message persisted
    "finish",  # loop finished (terminal); carries usage + reason
    "error",  # unrecoverable error
    # --- ReAct lifecycle events (Thought → Action → Observation) ---
    "react_thought",  # explicit Thought phase (reasoning before action)
    "react_action",  # explicit Action phase (tool invocation intent)
    "react_observation",  # explicit Observation phase (tool result interpretation)
]


@dataclass
class AgentEvent:
    kind: EventKind
    # Free-form payload, shape depends on kind:
    #   start:            {"conversation_id": int | None, "run_id": int | None}
    #   thinking:         {"text": str}
    #   token:            {"text": str}
    #   tool_call_start:  {"id": str, "name": str, "arguments": dict}
    #   tool_approval_request: {"id": str, "name": str, "arguments": dict,
    #                           "reason": str, "requires_decision": True}
    #   tool_result:      {"id": str, "name": str, "result": ToolResult-dict}
    #   message:          {"role": "assistant", "content": str, "tool_calls": ...,
    #                      "thinking": str | None}
    #   finish:           {"reason": str, "usage": Usage-dict | None, "iterations": int,
    #                      "elapsed_ms": int | None}
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
    def start(
        cls,
        *,
        conversation_id: int | None = None,
        run_id: int | None = None,
    ) -> AgentEvent:
        return cls(
            kind="start",
            payload={"conversation_id": conversation_id, "run_id": run_id},
        )

    @classmethod
    def thinking(cls, text: str) -> AgentEvent:
        return cls(kind="thinking", payload={"text": text})

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
    def tool_approval_request(
        cls,
        *,
        call_id: str,
        name: str,
        arguments: dict[str, Any],
        reason: str = "Tool requires approval",
    ) -> AgentEvent:
        """Emit when a tool call is gated behind human approval.

        The client must POST its decision to the approval endpoint; the loop is
        blocked on the matching approval Future until then. ``requires_decision``
        is always True so clients can branch on a stable boolean.
        """
        return cls(
            kind="tool_approval_request",
            payload={
                "id": call_id,
                "name": name,
                "arguments": arguments,
                "reason": reason,
                "requires_decision": True,
            },
        )

    @classmethod
    def tool_result(cls, *, call_id: str, name: str, result: Any) -> AgentEvent:
        return cls(
            kind="tool_result",
            payload={"id": call_id, "name": name, "result": result},
        )

    @classmethod
    def message(
        cls,
        *,
        content: str | None,
        tool_calls: list[dict] | None,
        thinking: str | None = None,
    ) -> AgentEvent:
        return cls(
            kind="message",
            payload={
                "role": "assistant",
                "content": content,
                "tool_calls": tool_calls,
                "thinking": thinking,
            },
        )

    @classmethod
    def finish(
        cls,
        *,
        reason: str,
        usage: Usage | None,
        iterations: int,
        elapsed_ms: int | None = None,
    ) -> AgentEvent:
        return cls(
            kind="finish",
            payload={
                "reason": reason,
                "usage": vars(usage) if usage else None,
                "iterations": iterations,
                "elapsed_ms": elapsed_ms,
            },
        )

    @classmethod
    def error(cls, message: str, detail: str | None = None) -> AgentEvent:
        return cls(kind="error", payload={"message": message, "detail": detail})

    # --- ReAct lifecycle constructors ---

    @classmethod
    def react_thought(cls, *, step: int, text: str) -> AgentEvent:
        """Explicit Thought phase: the model's reasoning before taking action."""
        return cls(kind="react_thought", payload={"step": step, "text": text})

    @classmethod
    def react_action(cls, *, step: int, tool_name: str, arguments: dict[str, Any], call_id: str) -> AgentEvent:
        """Explicit Action phase: the model decides to invoke a tool."""
        return cls(
            kind="react_action",
            payload={"step": step, "tool_name": tool_name, "arguments": arguments, "call_id": call_id},
        )

    @classmethod
    def react_observation(cls, *, step: int, tool_name: str, result_summary: str, is_error: bool = False) -> AgentEvent:
        """Explicit Observation phase: the result of the action is interpreted."""
        return cls(
            kind="react_observation",
            payload={"step": step, "tool_name": tool_name, "result_summary": result_summary, "is_error": is_error},
        )
