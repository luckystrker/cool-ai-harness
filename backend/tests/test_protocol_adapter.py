"""M1 compatibility and schema drift gates for App Protocol v1."""

from __future__ import annotations

import json
from pathlib import Path
from typing import get_args

import pytest
from jsonschema import Draft202012Validator

from app.agent.events import AgentEvent, EventKind
from app.protocol import AGENT_EVENT_KINDS, RESEARCH_EVENT_TYPES, CanonicalEventAdapter

ROOT = Path(__file__).parents[2]
SCHEMA = json.loads((ROOT / "schemas" / "cool-protocol-v1.schema.json").read_text())
EVENT_SCHEMA = {
    "$schema": SCHEMA["$schema"],
    "$ref": "#/$defs/EventEnvelope",
    "$defs": SCHEMA["$defs"],
}
EVENT_VALIDATOR = Draft202012Validator(EVENT_SCHEMA)


def test_every_agent_event_kind_has_a_canonical_mapping() -> None:
    assert set(get_args(EventKind)) == AGENT_EVENT_KINDS
    adapter = CanonicalEventAdapter(clock=lambda: "2026-08-31T00:00:00Z")
    for kind in sorted(AGENT_EVENT_KINDS):
        payload = {
            "tool_approval_request": {
                "id": "call-1",
                "name": "write_file",
                "arguments": {},
                "approval_id": "approval-1",
                "revision": 1,
            },
            "tool_approval_resolved": {
                "id": "call-1",
                "approval_id": "approval-1",
                "revision": 1,
                "decision": "approved",
            },
            "plan_progress": {
                "plan_id": 1,
                "completed": 0,
                "total": 1,
                "current_step": None,
                "status": "executing",
            },
        }.get(kind, {})
        envelope = adapter.adapt_agent_event(kind, payload)
        EVENT_VALIDATOR.validate(envelope)


@pytest.mark.parametrize(
    ("kind", "payload"),
    [
        ("tool_approval_request", {"approval_id": "", "revision": 1}),
        ("tool_approval_request", {"approval_id": "approval-1", "revision": 0}),
        ("tool_approval_resolved", {"approval_id": "approval-1", "revision": 1}),
        (
            "tool_approval_resolved",
            {"approval_id": "approval-1", "revision": 1, "decision": "maybe"},
        ),
    ],
)
def test_approval_adapter_rejects_missing_or_invalid_server_identity(
    kind: str, payload: dict
) -> None:
    adapter = CanonicalEventAdapter(clock=lambda: "2026-08-31T00:00:00Z")
    with pytest.raises(ValueError):
        adapter.adapt_agent_event(kind, payload)


def test_plan_progress_adapter_requires_explicit_terminal_capable_status() -> None:
    adapter = CanonicalEventAdapter(clock=lambda: "2026-08-31T00:00:00Z")
    with pytest.raises(ValueError):
        adapter.adapt_agent_event(
            "plan_progress", {"plan_id": 1, "completed": 0, "total": 1}
        )


def test_agent_adapter_roundtrip_is_lossless_and_isolated() -> None:
    payload = {"id": "call-1", "name": "write_file", "arguments": {"text": "Привет"}}
    adapter = CanonicalEventAdapter(clock=lambda: "2026-08-31T00:00:00Z")
    envelope = adapter.adapt_agent_event("tool_call_start", payload)
    EVENT_VALIDATOR.validate(envelope)
    projected = adapter.project_agent_event(envelope)
    assert projected == {"kind": "tool_call_start", "payload": payload}
    projected["payload"]["arguments"]["text"] = "changed"
    assert payload["arguments"]["text"] == "Привет"


def test_run_scoped_adapter_assigns_unique_monotonic_event_identity() -> None:
    adapter = CanonicalEventAdapter(
        session_id="conversation:4",
        run_id="run:9",
        actor_id="user:2",
        clock=lambda: "2026-08-31T00:00:00Z",
    )
    first = adapter.adapt_agent_event("start", {"conversation_id": 4, "run_id": 9})
    second = adapter.adapt_agent_event("token", {"text": "hello"})
    assert [first["seq"], second["seq"]] == [1, 2]
    assert first["eventId"] != second["eventId"]
    assert first["sessionId"] == second["sessionId"] == "conversation:4"
    assert first["runId"] == second["runId"] == "run:9"
    assert first["actor"] == {"id": "user:2", "kind": "local_user"}
    EVENT_VALIDATOR.validate(first)
    EVENT_VALIDATOR.validate(second)


def test_every_research_event_has_a_canonical_mapping_and_lossless_projection() -> None:
    adapter = CanonicalEventAdapter(clock=lambda: "2026-08-31T00:00:00Z")
    for type_ in sorted(RESEARCH_EVENT_TYPES):
        payload = {"run_id": 7, "marker": type_}
        envelope = adapter.adapt_research_event(type_, payload)
        EVENT_VALIDATOR.validate(envelope)
        assert adapter.project_research_event(envelope) == {"type": type_, "payload": payload}

    completed = adapter.adapt_research_event(
        "completed",
        {"run_id": 7, "report_artifact_id": 21, "sources_count": 5, "citations_count": 3},
    )
    assert completed["event"] == {
        "kind": "research.completed",
        "payload": {"artifactId": "21", "sourceCount": 5, "error": None},
    }


def test_committed_golden_traces_match_schema() -> None:
    files = sorted((ROOT / "crates" / "cool-protocol" / "tests" / "golden").glob("*.json"))
    assert len(files) == 12
    for file in files:
        trace = json.loads(file.read_text())
        for event in trace["events"]:
            EVENT_VALIDATOR.validate(event)


def test_current_agent_wire_shape_passes_through_adapter_unchanged() -> None:
    event = AgentEvent.token("токен")
    adapter = CanonicalEventAdapter(session_id="conversation:1", run_id="run:1")
    event.bind_canonical(adapter)
    assert event.to_dict() == {"kind": "token", "payload": {"text": "токен"}}
    assert json.loads(event.to_dict_json()) == event.to_dict()
    assert event.to_canonical_dict()["seq"] == 1


def test_event_schema_rejects_unknown_top_level_fields_and_wrong_version() -> None:
    adapter = CanonicalEventAdapter(clock=lambda: "2026-08-31T00:00:00Z")
    envelope = adapter.adapt_agent_event("token", {"text": "hello"})
    envelope["rogue"] = True
    assert list(EVENT_VALIDATOR.iter_errors(envelope))
    envelope.pop("rogue")
    envelope["event"]["rogue"] = True
    assert list(EVENT_VALIDATOR.iter_errors(envelope))
    envelope["event"].pop("rogue")
    envelope["schemaVersion"] = 2
    assert list(EVENT_VALIDATOR.iter_errors(envelope))
