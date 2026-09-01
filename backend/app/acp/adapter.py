"""Projection from canonical App Protocol events to ACP v1 updates.

The adapter deliberately consumes canonical envelopes, not executor callbacks.
That keeps ACP a transport projection over the same event stream used by the
Web API and durable ``run_events`` log.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from copy import deepcopy
from typing import Any

from app.protocol.adapter import EXTENSION_NAMESPACE


def _mapping(value: Any) -> dict[str, Any]:
    return dict(value) if isinstance(value, Mapping) else {}


def _text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return json.dumps(value, default=str, ensure_ascii=False, sort_keys=True)


def _tool_kind(name: str) -> str:
    lowered = name.lower()
    if any(part in lowered for part in ("delete", "remove")):
        return "delete"
    if any(part in lowered for part in ("move", "rename")):
        return "move"
    if any(part in lowered for part in ("write", "edit", "patch", "create_file")):
        return "edit"
    if any(part in lowered for part in ("read", "list", "glob")):
        return "read"
    if any(part in lowered for part in ("search", "grep", "find")):
        return "search"
    if any(part in lowered for part in ("bash", "shell", "execute", "python", "terminal")):
        return "execute"
    if any(part in lowered for part in ("http", "web", "fetch", "rss")):
        return "fetch"
    if "plan" in lowered:
        return "think"
    return "other"


def _legacy_payload(envelope: Mapping[str, Any]) -> dict[str, Any]:
    extensions = _mapping(envelope.get("extensions"))
    namespace = _mapping(extensions.get(EXTENSION_NAMESPACE))
    legacy = _mapping(namespace.get("legacyAgentEvent"))
    return _mapping(legacy.get("payload"))


class ACPEventAdapter:
    """Stateful ACP projection for one prompt turn."""

    def __init__(self) -> None:
        self._known_tools: dict[str, str] = {}
        self._plan_entries: list[dict[str, str]] = []
        self._saw_content_delta = False

    def adapt(self, envelope: Mapping[str, Any]) -> list[dict[str, Any]]:
        event = _mapping(envelope.get("event"))
        kind = str(event.get("kind", ""))
        payload = _mapping(event.get("payload"))

        if kind == "content.delta":
            self._saw_content_delta = True
            return [self.message_chunk(_text(payload.get("text")), role="agent")]
        if kind == "reasoning.delta":
            return [
                {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": _text(payload.get("text"))},
                }
            ]
        if kind == "tool.requested":
            call_id = str(payload.get("callId") or "legacy-tool-call")
            name = str(payload.get("name") or "Tool")
            if call_id in self._known_tools:
                return [
                    {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call_id,
                        "rawInput": deepcopy(payload.get("arguments")),
                    }
                ]
            self._known_tools[call_id] = name
            return [
                {
                    "sessionUpdate": "tool_call",
                    "toolCallId": call_id,
                    "title": name,
                    "kind": _tool_kind(name),
                    "status": "pending",
                    "rawInput": deepcopy(payload.get("arguments")),
                }
            ]
        if kind == "tool.approval_required":
            return [self.tool_update(payload, status="pending")]
        if kind == "tool.approval_resolved":
            decision = str(payload.get("decision") or "denied")
            return [
                self.tool_update(
                    payload,
                    status="in_progress" if decision == "approved" else "failed",
                    raw_output={"permissionDecision": decision},
                )
            ]
        if kind in {"tool.completed", "tool.failed"}:
            raw_output = payload.get("result")
            if kind == "tool.failed":
                raw_output = {
                    "errorCode": payload.get("errorCode"),
                    "message": payload.get("message"),
                }
            return [
                self.tool_update(
                    payload,
                    status="completed" if kind == "tool.completed" else "failed",
                    raw_output=raw_output,
                )
            ]
        if kind == "item.completed" and not self._saw_content_delta:
            content = _text(payload.get("content"))
            return [self.message_chunk(content, role="agent")] if content else []
        if kind == "usage.updated":
            # Canonical usage is per run/call, while ACP v1 usage represents
            # current session context.  Do not publish misleading arithmetic;
            # a later context-window authority can add this projection.
            return []
        if kind.startswith("plan."):
            return self._adapt_plan(kind, payload, _legacy_payload(envelope))
        return []

    @staticmethod
    def message_chunk(text: str, *, role: str) -> dict[str, Any]:
        return {
            "sessionUpdate": f"{role}_message_chunk",
            "content": {"type": "text", "text": text},
        }

    def tool_update(
        self,
        payload: Mapping[str, Any],
        *,
        status: str,
        raw_output: Any | None = None,
    ) -> dict[str, Any]:
        call_id = str(payload.get("callId") or "legacy-tool-call")
        update: dict[str, Any] = {
            "sessionUpdate": "tool_call_update",
            "toolCallId": call_id,
            "status": status,
        }
        if raw_output is not None:
            update["rawOutput"] = deepcopy(raw_output)
        return update

    def permission_tool_call(self, payload: Mapping[str, Any]) -> dict[str, Any]:
        call_id = str(payload.get("callId") or "legacy-tool-call")
        name = str(payload.get("name") or self._known_tools.get(call_id) or "Tool")
        return {
            "toolCallId": call_id,
            "title": name,
            "kind": _tool_kind(name),
            "status": "pending",
            "rawInput": deepcopy(payload.get("arguments")),
        }

    def _adapt_plan(
        self,
        kind: str,
        payload: Mapping[str, Any],
        legacy: Mapping[str, Any],
    ) -> list[dict[str, Any]]:
        if kind == "plan.created":
            steps = legacy.get("steps")
            self._plan_entries = []
            if isinstance(steps, list):
                for index, raw in enumerate(steps):
                    step = _mapping(raw)
                    content = str(
                        step.get("title") or step.get("description") or f"Step {index + 1}"
                    )
                    self._plan_entries.append(
                        {"content": content, "priority": "medium", "status": "pending"}
                    )
        elif kind in {"plan.step_started", "plan.step_completed"}:
            position = int(payload.get("position") or 0)
            index = position - 1 if position > 0 else position
            while len(self._plan_entries) <= index:
                self._plan_entries.append(
                    {
                        "content": f"Step {len(self._plan_entries) + 1}",
                        "priority": "medium",
                        "status": "pending",
                    }
                )
            entry = self._plan_entries[index]
            if payload.get("title"):
                entry["content"] = str(payload["title"])
            entry["status"] = "in_progress" if kind == "plan.step_started" else "completed"
        elif kind == "plan.progress" and str(payload.get("status")) == "completed":
            for entry in self._plan_entries:
                entry["status"] = "completed"

        if not self._plan_entries:
            return []
        return [
            {
                "sessionUpdate": "plan",
                "entries": deepcopy(self._plan_entries),
            }
        ]
