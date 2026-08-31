"""Agent loop events.

The AgentExecutor yields a stream of AgentEvent objects. They are the single
source of truth that the API layer (SSE / WebSocket / Telegram) translates
into whatever wire format the transport needs. Keeping them transport-agnostic
means the same loop drives chat, subagents, and cron jobs (Фаза 3b).
"""

from __future__ import annotations

from copy import deepcopy
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
    "tool_approval_resolved",  # server-owned approval reached approved/denied/timed_out
    "tool_result",  # tool finished with its ToolResult
    "message",  # a complete assistant message persisted
    "finish",  # loop finished (terminal); carries usage + reason
    "error",  # unrecoverable error
    # --- Cost budgets (Фаза 1.5 §5) ---
    "budget_alert",  # spend crossed the alert threshold (e.g. 80 %)
    # --- ReAct lifecycle events (Thought → Action → Observation) ---
    "react_thought",  # explicit Thought phase (reasoning before action)
    "react_action",  # explicit Action phase (tool invocation intent)
    "react_observation",  # explicit Observation phase (tool result interpretation)
    # --- Inspector / per-iteration metrics (Фаза 1.5 §6) ---
    "llm_call_complete",  # one LLM round-trip finished; carries timing + usage
    # --- Planning Mode (Фаза 2 §1) ---
    "plan_generated",  # LLM produced a structured plan; carries full plan JSON
    "plan_step_start",  # a plan step began execution
    "plan_step_complete",  # a plan step finished (success/failure/skipped)
    "plan_progress",  # overall plan progress update
    # --- Subagents (Фаза 2 §5) ---
    "subagent_started",  # a subagent was launched
    "subagent_progress",  # subagent iteration/content update
    "subagent_completed",  # subagent finished successfully
    "subagent_failed",  # subagent failed
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
    _canonical_envelope: dict[str, Any] | None = field(default=None, init=False, repr=False)

    def bind_canonical(self, adapter: Any) -> AgentEvent:
        """Bind this event to one run-scoped canonical sequencer."""
        self._canonical_envelope = adapter.adapt_agent_event(self.kind, self.payload)
        return self

    def to_canonical_dict(self) -> dict[str, Any]:
        if self._canonical_envelope is None:
            raise RuntimeError("AgentEvent is not bound to a run-scoped canonical adapter")
        return deepcopy(self._canonical_envelope)

    def to_dict(self) -> dict[str, Any]:
        if self._canonical_envelope is None:
            return {"kind": self.kind, "payload": self.payload}
        from app.protocol import CanonicalEventAdapter

        return CanonicalEventAdapter.project_agent_event(self._canonical_envelope)

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
    def tool_approval_resolved(
        cls,
        *,
        call_id: str,
        approval_id: str,
        revision: int,
        decision: str,
    ) -> AgentEvent:
        return cls(
            kind="tool_approval_resolved",
            payload={
                "id": call_id,
                "approval_id": approval_id,
                "revision": revision,
                "decision": decision,
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

    # --- Cost budget constructors (Фаза 1.5 §5) ---

    @classmethod
    def budget_alert(
        cls,
        *,
        window: str,
        spend_usd: float,
        limit_usd: float,
        pct: float,
    ) -> AgentEvent:
        """Emit when spend crosses the alert threshold for a budget window."""
        return cls(
            kind="budget_alert",
            payload={
                "window": window,
                "spend_usd": spend_usd,
                "limit_usd": limit_usd,
                "pct": pct,
            },
        )

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

    # --- Inspector constructors (Фаза 1.5 §6) ---

    @classmethod
    def llm_call_complete(
        cls,
        *,
        iteration: int,
        model: str,
        usage: dict[str, Any] | None,
        duration_ms: int,
    ) -> AgentEvent:
        """One LLM round-trip completed. Carries per-iteration timing and usage."""
        return cls(
            kind="llm_call_complete",
            payload={
                "iteration": iteration,
                "model": model,
                "usage": usage,
                "duration_ms": duration_ms,
            },
        )

    # --- Planning Mode constructors (Фаза 2 §1) ---

    @classmethod
    def plan_generated(cls, *, plan_id: int, title: str | None, steps: list[dict[str, Any]]) -> AgentEvent:
        """LLM produced a structured plan awaiting user review."""
        return cls(
            kind="plan_generated",
            payload={"plan_id": plan_id, "title": title, "steps": steps},
        )

    @classmethod
    def plan_step_start(cls, *, plan_id: int, position: int, title: str) -> AgentEvent:
        """A plan step began execution."""
        return cls(
            kind="plan_step_start",
            payload={"plan_id": plan_id, "position": position, "title": title},
        )

    @classmethod
    def plan_step_complete(
        cls, *, plan_id: int, position: int, status: str, result_summary: str | None = None
    ) -> AgentEvent:
        """A plan step finished execution."""
        return cls(
            kind="plan_step_complete",
            payload={
                "plan_id": plan_id,
                "position": position,
                "status": status,
                "result_summary": result_summary,
            },
        )

    @classmethod
    def plan_progress(
        cls,
        *,
        plan_id: int,
        completed: int,
        total: int,
        status: str,
        current_step: int | None = None,
    ) -> AgentEvent:
        """Overall plan progress update."""
        return cls(
            kind="plan_progress",
            payload={
                "plan_id": plan_id,
                "completed": completed,
                "total": total,
                "current_step": current_step,
                "status": status,
            },
        )

    # --- Subagent constructors (Фаза 2 §5) ---

    @classmethod
    def subagent_started(
        cls, *, subagent_run_id: int, name: str | None, role: str | None, prompt: str
    ) -> AgentEvent:
        """A subagent was launched."""
        return cls(
            kind="subagent_started",
            payload={
                "subagent_run_id": subagent_run_id,
                "name": name,
                "role": role,
                "prompt": prompt,
            },
        )

    @classmethod
    def subagent_progress(
        cls, *, subagent_run_id: int, iteration: int, content_delta: str | None = None
    ) -> AgentEvent:
        """Subagent iteration/content update."""
        return cls(
            kind="subagent_progress",
            payload={
                "subagent_run_id": subagent_run_id,
                "iteration": iteration,
                "content_delta": content_delta,
            },
        )

    @classmethod
    def subagent_completed(
        cls, *, subagent_run_id: int, result_summary: str | None, usage: dict | None = None
    ) -> AgentEvent:
        """Subagent finished successfully."""
        return cls(
            kind="subagent_completed",
            payload={
                "subagent_run_id": subagent_run_id,
                "result_summary": result_summary,
                "usage": usage,
            },
        )

    @classmethod
    def subagent_failed(cls, *, subagent_run_id: int, error: str) -> AgentEvent:
        """Subagent failed."""
        return cls(
            kind="subagent_failed",
            payload={"subagent_run_id": subagent_run_id, "error": error},
        )
