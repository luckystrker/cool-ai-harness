"""Lossless adapters from legacy Python streams to App Protocol v1.

M1 deliberately preserves the existing SPA wire shapes. Every legacy event is
first represented as a typed canonical envelope and the exact legacy object is
stored in the product extension namespace. Projecting that extension produces
the byte-equivalent logical object consumed by existing clients.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from copy import deepcopy
from datetime import UTC, datetime
from typing import Any, Final

PROTOCOL_VERSION: Final = 1
SCHEMA_VERSION: Final = 1
EXTENSION_NAMESPACE: Final = "io.github.luckystrker.cool"

AGENT_EVENT_KINDS: Final = frozenset(
    {
        "start",
        "thinking",
        "token",
        "tool_call_start",
        "tool_call_delta",
        "tool_approval_request",
        "tool_approval_resolved",
        "tool_result",
        "message",
        "finish",
        "error",
        "budget_alert",
        "react_thought",
        "react_action",
        "react_observation",
        "llm_call_complete",
        "plan_generated",
        "plan_step_start",
        "plan_step_complete",
        "plan_progress",
        "subagent_started",
        "subagent_progress",
        "subagent_completed",
        "subagent_failed",
    }
)

RESEARCH_EVENT_TYPES: Final = frozenset(
    {
        "started",
        "stage",
        "source_found",
        "subquestion_started",
        "subquestion_completed",
        "completed",
        "failed",
        "cancelled",
    }
)


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def _string(value: Any, fallback: str = "") -> str:
    return fallback if value is None else str(value)


def _mapping(value: Any) -> dict[str, Any]:
    return dict(value) if isinstance(value, Mapping) else {}


def _number(value: Any, fallback: float = 0.0) -> float:
    return float(value) if isinstance(value, int | float) else fallback


def _required_string(payload: Mapping[str, Any], key: str) -> str:
    value = payload.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{key} must be a non-empty server-issued string")
    return value


def _required_positive_int(payload: Mapping[str, Any], key: str) -> int:
    value = payload.get(key)
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        raise ValueError(f"{key} must be a positive integer")
    return value


class CanonicalEventAdapter:
    """Stateful event sequencer for one legacy run stream."""

    def __init__(
        self,
        *,
        session_id: str = "legacy-session",
        run_id: str = "legacy-run",
        source: str = "python-compat",
        actor_id: str = "local-user",
        clock: Callable[[], str] = _utc_now,
        start_seq: int = 0,
    ) -> None:
        self.session_id = session_id
        self.run_id = run_id
        self.source = source
        self.actor_id = actor_id
        self._clock = clock
        self._seq = start_seq

    def adapt_agent_event(self, kind: str, payload: Mapping[str, Any]) -> dict[str, Any]:
        if kind not in AGENT_EVENT_KINDS:
            raise ValueError(f"unmapped AgentEvent kind: {kind}")
        legacy = {"kind": kind, "payload": deepcopy(dict(payload))}
        event_kind, canonical_payload = _agent_payload(kind, payload)
        return self._envelope(event_kind, canonical_payload, "legacyAgentEvent", legacy)

    def adapt_research_event(self, type_: str, payload: Mapping[str, Any]) -> dict[str, Any]:
        if type_ not in RESEARCH_EVENT_TYPES:
            raise ValueError(f"unmapped research event type: {type_}")
        legacy = {"type": type_, "payload": deepcopy(dict(payload))}
        event_kind, canonical_payload = _research_payload(type_, payload)
        return self._envelope(event_kind, canonical_payload, "legacyResearchEvent", legacy)

    def _envelope(
        self,
        kind: str,
        payload: dict[str, Any],
        extension_key: str,
        legacy: dict[str, Any],
    ) -> dict[str, Any]:
        self._seq += 1
        return {
            "eventId": f"{self.run_id}:{self._seq}",
            "schemaVersion": SCHEMA_VERSION,
            "sessionId": self.session_id,
            "runId": self.run_id,
            "itemId": None,
            "seq": self._seq,
            "occurredAt": self._clock(),
            "actor": {"id": self.actor_id, "kind": "local_user"},
            "source": self.source,
            "causationId": None,
            "correlationId": None,
            "event": {"kind": kind, "payload": payload},
            "extensions": {EXTENSION_NAMESPACE: {extension_key: legacy}},
        }

    @staticmethod
    def project_agent_event(envelope: Mapping[str, Any]) -> dict[str, Any]:
        return _project(envelope, "legacyAgentEvent")

    @staticmethod
    def project_research_event(envelope: Mapping[str, Any]) -> dict[str, Any]:
        return _project(envelope, "legacyResearchEvent")


def _project(envelope: Mapping[str, Any], key: str) -> dict[str, Any]:
    extensions = _mapping(envelope.get("extensions"))
    namespace = _mapping(extensions.get(EXTENSION_NAMESPACE))
    legacy = namespace.get(key)
    if not isinstance(legacy, Mapping):
        raise ValueError(f"canonical event has no {EXTENSION_NAMESPACE}/{key} extension")
    return deepcopy(dict(legacy))


def _agent_payload(kind: str, payload: Mapping[str, Any]) -> tuple[str, dict[str, Any]]:
    if kind == "start":
        return "run.started", {"model": None, "mode": None}
    if kind in {"thinking", "react_thought"}:
        return "reasoning.delta", {"text": _string(payload.get("text")), "channel": kind}
    if kind == "token":
        return "content.delta", {"text": _string(payload.get("text")), "channel": "final"}
    if kind in {"tool_call_start", "tool_call_delta", "react_action"}:
        call_id = payload.get("id", payload.get("call_id", "legacy-tool-call"))
        name = payload.get("name", payload.get("tool_name", "unknown"))
        return "tool.requested", {
            "callId": _string(call_id, "legacy-tool-call"),
            "name": _string(name, "unknown"),
            "arguments": _mapping(payload.get("arguments")),
        }
    if kind == "tool_approval_request":
        call_id = _string(payload.get("id"), "legacy-tool-call")
        return "tool.approval_required", {
            "callId": call_id,
            "name": _string(payload.get("name"), "unknown"),
            "arguments": _mapping(payload.get("arguments")),
            "reason": _string(payload.get("reason"), "Tool requires approval"),
            "approvalId": _required_string(payload, "approval_id"),
            "revision": _required_positive_int(payload, "revision"),
            "breakpointType": _string(payload.get("breakpoint_type")) or None,
            "resultPreview": _string(payload.get("result_preview")) or None,
            "currentContent": _string(payload.get("current_content")) or None,
        }
    if kind == "tool_approval_resolved":
        decision = _required_string(payload, "decision")
        if decision not in {"approved", "denied", "timed_out"}:
            raise ValueError("decision must be approved, denied, or timed_out")
        return "tool.approval_resolved", {
            "callId": _string(payload.get("id"), "legacy-tool-call"),
            "approvalId": _required_string(payload, "approval_id"),
            "revision": _required_positive_int(payload, "revision"),
            "decision": decision,
        }
    if kind == "tool_result":
        result = payload.get("result")
        result_map = _mapping(result)
        failed = bool(result_map.get("is_error")) or result_map.get("success") is False
        common = {
            "callId": _string(payload.get("id"), "legacy-tool-call"),
            "name": _string(payload.get("name"), "unknown"),
        }
        if failed:
            return "tool.failed", {
                **common,
                "errorCode": _string(result_map.get("error_code"), "tool_failed"),
                "message": _string(result_map.get("error", result_map.get("output"))) or None,
            }
        return "tool.completed", {**common, "result": deepcopy(result)}
    if kind == "message":
        return "item.completed", {
            "role": _string(payload.get("role"), "assistant"),
            "content": _string(payload.get("content")) or None,
            "toolCalls": [],
        }
    if kind == "finish":
        return "run.completed", {"reason": _string(payload.get("reason"), "completed"), "errorCode": None}
    if kind == "error":
        return "run.failed", {
            "reason": _string(payload.get("message"), "Agent run failed"),
            "errorCode": "agent_error",
        }
    if kind == "budget_alert":
        return "budget.warning", {
            "window": _string(payload.get("window"), "run"),
            "spendUsd": _number(payload.get("spend_usd")),
            "limitUsd": _number(payload.get("limit_usd")),
            "percent": _number(payload.get("pct")),
        }
    if kind == "react_observation":
        return "item.updated", {
            "role": "tool",
            "content": _string(payload.get("result_summary")) or None,
            "toolCalls": [],
        }
    if kind == "llm_call_complete":
        usage = _mapping(payload.get("usage"))
        prompt = int(usage.get("prompt_tokens", usage.get("input_tokens", 0)) or 0)
        completion = int(usage.get("completion_tokens", usage.get("output_tokens", 0)) or 0)
        return "usage.updated", {
            "promptTokens": prompt,
            "completionTokens": completion,
            "totalTokens": int(usage.get("total_tokens", prompt + completion) or 0),
            "costUsd": _number(usage.get("cost_usd")) if usage.get("cost_usd") is not None else None,
        }
    if kind == "plan_generated":
        steps = payload.get("steps")
        return "plan.created", {
            "planId": _string(payload.get("plan_id"), "legacy-plan"),
            "title": _string(payload.get("title")) or None,
            "totalSteps": len(steps) if isinstance(steps, list) else 0,
        }
    if kind in {"plan_step_start", "plan_step_complete"}:
        return ("plan.step_started" if kind == "plan_step_start" else "plan.step_completed"), {
            "planId": _string(payload.get("plan_id"), "legacy-plan"),
            "position": int(payload.get("position", 0) or 0),
            "title": _string(payload.get("title"), "Plan step"),
            "status": _string(payload.get("status"), "running" if kind == "plan_step_start" else "completed"),
            "resultSummary": _string(payload.get("result_summary")) or None,
        }
    if kind == "plan_progress":
        status = _required_string(payload, "status")
        if status not in {"executing", "completed", "failed"}:
            raise ValueError("plan progress status must be executing, completed, or failed")
        return "plan.progress", {
            "planId": _string(payload.get("plan_id"), "legacy-plan"),
            "completedSteps": int(payload.get("completed", 0) or 0),
            "totalSteps": int(payload.get("total", 0) or 0),
            "message": (
                f"current step {payload['current_step']}"
                if payload.get("current_step") is not None
                else None
            ),
            "status": status,
        }
    if kind == "subagent_progress":
        return "subagent.progress", {
            "subagentRunId": _string(payload.get("subagent_run_id"), "legacy-subagent"),
            "message": _string(payload.get("content_delta"), f"iteration {payload.get('iteration', 0)}"),
            "percent": None,
        }
    if kind in {"subagent_started", "subagent_completed", "subagent_failed"}:
        status = {
            "subagent_started": "running",
            "subagent_completed": "completed",
            "subagent_failed": "failed",
        }[kind]
        return f"subagent.{status if status != 'running' else 'started'}", {
            "subagentRunId": _string(payload.get("subagent_run_id"), "legacy-subagent"),
            "name": _string(payload.get("name")) or None,
            "status": status,
            "summary": _string(payload.get("result_summary")) or None,
            "error": _string(payload.get("error")) or None,
        }
    raise AssertionError(f"coverage error for AgentEvent kind: {kind}")


def _research_payload(type_: str, payload: Mapping[str, Any]) -> tuple[str, dict[str, Any]]:
    if type_ == "started":
        return "research.started", {
            "researchRunId": _string(payload.get("run_id"), "legacy-research"),
        }
    if type_ == "stage":
        return "research.stage", {
            "stage": _string(payload.get("stage"), "unknown"),
            "message": _string(payload.get("message")) or None,
            "progress": _number(payload.get("progress")) if payload.get("progress") is not None else None,
        }
    if type_ == "source_found":
        return "research.source_found", {
            "url": _string(payload.get("url")),
            "title": _string(payload.get("title")) or None,
            "snippet": _string(payload.get("snippet")) or None,
            "confidence": _number(payload.get("confidence")) if payload.get("confidence") is not None else None,
        }
    if type_ in {"subquestion_started", "subquestion_completed"}:
        return f"research.{type_}", {
            "index": int(payload.get("index", 0) or 0),
            "question": _string(payload.get("sub_question", payload.get("question"))),
            "status": _string(payload.get("status"), "running" if type_.endswith("started") else "completed"),
        }
    terminal_kind = {
        "completed": "research.completed",
        "failed": "research.failed",
        "cancelled": "research.cancelled",
    }[type_]
    return terminal_kind, {
        "artifactId": _string(
            payload.get("report_artifact_id", payload.get("artifact_id"))
        )
        or None,
        "sourceCount": int(payload.get("sources_count", payload.get("source_count", 0)) or 0),
        "error": _string(payload.get("error")) or None,
    }
