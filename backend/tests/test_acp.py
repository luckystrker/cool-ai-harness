"""M4 ACP v1 adapter, durable-runtime integration and client fixtures."""

from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
from copy import deepcopy
from pathlib import Path
from typing import Any

import pytest
from jsonschema import Draft202012Validator
from sqlmodel import Session

from app.acp import ACPConnection, ACPEventAdapter
from app.acp.stdio import decode_message
from app.agent.events import AgentEvent
from app.agent.service import (
    get_run,
    list_messages,
    list_run_events,
    list_runs,
    update_conversation,
)
from app.core.db import engine
from app.protocol import CanonicalEventAdapter
from app.providers import ChatStreamEvent, LLMProvider, Message, ToolSpec, Usage

ROOT = Path(__file__).parents[2]
ACP_SCHEMA = json.loads((ROOT / "schemas" / "acp-v1.schema.json").read_text(encoding="utf-8"))
ACP_VALIDATOR = Draft202012Validator(ACP_SCHEMA)


def _assert_acp_message(message: Any) -> None:
    errors = list(ACP_VALIDATOR.iter_errors(message))
    assert not errors, "\n".join(error.message for error in errors[:5])


async def _request(
    connection: ACPConnection,
    output: list[Any],
    *,
    request_id: str | int,
    method: str,
    params: dict[str, Any],
) -> dict[str, Any]:
    await connection.receive(
        {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
    )
    await connection.drain()
    response = next(item for item in reversed(output) if item.get("id") == request_id)
    _assert_acp_message(response)
    return response


async def _initialized_connection(
    output: list[Any], provider: LLMProvider | None = None
) -> ACPConnection:
    async def send(message: Any) -> None:
        output.append(message)

    kwargs: dict[str, Any] = {}
    if provider is not None:
        kwargs["provider_resolver"] = lambda _model: provider
    connection = ACPConnection(send, **kwargs)
    response = await _request(
        connection,
        output,
        request_id="init",
        method="initialize",
        params={
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "pytest", "version": "1"},
        },
    )
    assert response["result"]["protocolVersion"] == 1
    assert response["result"]["agentCapabilities"]["loadSession"] is True
    return connection


async def test_new_prompt_and_load_share_durable_conversation_and_event_log(
    scripted_provider, tmp_path: Path
) -> None:
    scripted_provider.set_script(["Hello from ACP"])
    output: list[Any] = []
    connection = await _initialized_connection(output, scripted_provider)
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    conversation_id = int(session_id.split(":", 1)[1])

    prompted = await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "Say hello"}],
        },
    )
    assert prompted["result"] == {"stopReason": "end_turn"}
    updates = [item for item in output if item.get("method") == "session/update"]
    assert "Hello " in [item["params"]["update"].get("content", {}).get("text") for item in updates]
    for update in updates:
        _assert_acp_message(update)

    with Session(engine) as session:
        assert [message.role for message in list_messages(session, conversation_id)] == [
            "user",
            "assistant",
        ]
        runs = list_runs(session, conversation_id=conversation_id)
        assert len(runs) == 1
        assert runs[0].id is not None
        durable_events = list_run_events(session, run_id=runs[0].id)
        assert [event.kind for event in durable_events][:2] == ["start", "token"]
        assert runs[0].status == "completed"

    replay_output: list[Any] = []
    replay = await _initialized_connection(replay_output, scripted_provider)
    loaded = await _request(
        replay,
        replay_output,
        request_id="load",
        method="session/load",
        params={"sessionId": session_id, "cwd": str(tmp_path), "mcpServers": []},
    )
    assert "result" in loaded
    replay_roles = [
        item["params"]["update"]["sessionUpdate"]
        for item in replay_output
        if item.get("method") == "session/update"
    ]
    assert replay_roles == ["user_message_chunk", "agent_message_chunk"]
    await replay.close()
    await connection.close()


async def test_permission_request_resolves_authoritative_approval(
    scripted_provider, tmp_path: Path
) -> None:
    scripted_provider.set_script(
        [
            [
                {
                    "id": "call-write",
                    "name": "write_file",
                    "arguments": {"path": "approved.txt", "content": "ok"},
                }
            ],
            "written",
        ]
    )
    output: list[Any] = []
    connection: ACPConnection

    async def send(message: Any) -> None:
        output.append(message)
        if message.get("method") == "session/request_permission":
            _assert_acp_message(message)
            await connection.receive(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}},
                }
            )

    connection = ACPConnection(send, provider_resolver=lambda _model: scripted_provider)
    await _request(
        connection,
        output,
        request_id="init",
        method="initialize",
        params={"protocolVersion": 1, "clientCapabilities": {}},
    )
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    conversation_id = int(session_id.split(":", 1)[1])
    with Session(engine) as session:
        update_conversation(session, conversation_id, permissions={"write_file": "ask"})

    response = await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]},
    )
    assert response["result"]["stopReason"] == "end_turn"
    assert (tmp_path / "approved.txt").read_text() == "ok"
    tool_statuses = [
        message["params"]["update"].get("status")
        for message in output
        if message.get("method") == "session/update"
        and message["params"]["update"]["sessionUpdate"] == "tool_call_update"
    ]
    assert "pending" in tool_statuses
    assert "in_progress" in tool_statuses
    assert "completed" in tool_statuses

    replay_output: list[Any] = []
    replay = await _initialized_connection(replay_output, scripted_provider)
    await _request(
        replay,
        replay_output,
        request_id="load",
        method="session/load",
        params={"sessionId": session_id, "cwd": str(tmp_path), "mcpServers": []},
    )
    replay_tool_updates = [
        item["params"]["update"]
        for item in replay_output
        if item.get("method") == "session/update"
        and item["params"]["update"].get("toolCallId") == "call-write"
    ]
    assert [update["sessionUpdate"] for update in replay_tool_updates] == [
        "tool_call",
        "tool_call_update",
    ]
    await replay.close()
    await connection.close()


class _SlowProvider(LLMProvider):
    name = "slow"
    default_model = "slow-model"

    def __init__(self) -> None:
        self.started = asyncio.Event()

    async def chat_completion(self, messages, *, model, tools=None, **kwargs):  # type: ignore[override]
        raise NotImplementedError

    async def chat_completion_stream(
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None = None,
        **kwargs: Any,
    ):
        self.started.set()
        yield ChatStreamEvent(delta="started")
        await asyncio.Event().wait()
        yield ChatStreamEvent(
            finish=True,
            usage=Usage(prompt_tokens=1, completion_tokens=1, total_tokens=2),
        )


async def test_cancel_notification_returns_cancelled_and_persists_cancelled_run(
    tmp_path: Path,
) -> None:
    provider = _SlowProvider()
    output: list[Any] = []
    connection: ACPConnection

    async def send(message: Any) -> None:
        output.append(message)
        update = message.get("params", {}).get("update", {})
        if update.get("sessionUpdate") == "agent_message_chunk":
            await connection.receive(
                {
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {"sessionId": message["params"]["sessionId"]},
                }
            )

    connection = ACPConnection(send, provider_resolver=lambda _model: provider)
    await _request(
        connection,
        output,
        request_id="init",
        method="initialize",
        params={"protocolVersion": 1, "clientCapabilities": {}},
    )
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    response = await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={"sessionId": session_id, "prompt": [{"type": "text", "text": "wait"}]},
    )
    assert response["result"] == {"stopReason": "cancelled"}
    conversation_id = int(session_id.split(":", 1)[1])
    with Session(engine) as session:
        run = list_runs(session, conversation_id=conversation_id)[0]
        assert get_run(session, run.id).status == "cancelled"  # type: ignore[arg-type,union-attr]
    await connection.close()


async def test_two_connections_cannot_prompt_same_session_concurrently(tmp_path: Path) -> None:
    provider = _SlowProvider()
    first_output: list[Any] = []
    second_output: list[Any] = []
    first = await _initialized_connection(first_output, provider)
    second = await _initialized_connection(second_output, provider)
    created = await _request(
        first,
        first_output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    await first.receive(
        {
            "jsonrpc": "2.0",
            "id": "first-prompt",
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "first"}],
            },
        }
    )
    await asyncio.wait_for(provider.started.wait(), timeout=1)

    second_response = await _request(
        second,
        second_output,
        request_id="second-prompt",
        method="session/prompt",
        params={"sessionId": session_id, "prompt": [{"type": "text", "text": "second"}]},
    )
    assert second_response["error"]["code"] == -32000

    await first.receive(
        {
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id},
        }
    )
    await first.drain()
    first_response = next(item for item in first_output if item.get("id") == "first-prompt")
    assert first_response["result"]["stopReason"] == "cancelled"
    await second.close()
    await first.close()


async def test_load_rejects_mismatched_cwd_without_mutating_session(
    scripted_provider, tmp_path: Path
) -> None:
    original = tmp_path / "original"
    other = tmp_path / "other"
    original.mkdir()
    other.mkdir()
    output: list[Any] = []
    connection = await _initialized_connection(output, scripted_provider)
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(original), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    response = await _request(
        connection,
        output,
        request_id="load",
        method="session/load",
        params={"sessionId": session_id, "cwd": str(other), "mcpServers": []},
    )
    assert response["error"]["code"] == -32602
    conversation_id = int(session_id.split(":", 1)[1])
    with Session(engine) as session:
        from app.agent.service import get_conversation

        conversation = get_conversation(session, conversation_id)
        assert conversation is not None
        assert Path(conversation.working_directory or "").resolve() == original.resolve()
    await connection.close()


async def test_resource_link_is_preserved_without_implicit_fetch(
    scripted_provider, tmp_path: Path
) -> None:
    scripted_provider.set_script(["ok"])
    output: list[Any] = []
    connection = await _initialized_connection(output, scripted_provider)
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={
            "sessionId": session_id,
            "prompt": [
                {"type": "text", "text": "Inspect"},
                {
                    "type": "resource_link",
                    "name": "notes",
                    "uri": "https://example.invalid/notes.txt",
                },
            ],
        },
    )
    prompt = scripted_provider.calls[0][-1].content
    assert isinstance(prompt, str)
    assert "[ACP resource: notes](https://example.invalid/notes.txt)" in prompt
    await connection.close()


async def test_provider_resolution_failure_does_not_create_orphan_message_or_run(
    scripted_provider, tmp_path: Path
) -> None:
    output: list[Any] = []
    connection = await _initialized_connection(output, scripted_provider)
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    conversation_id = int(session_id.split(":", 1)[1])
    connection._provider_resolver = lambda _model: (_ for _ in ()).throw(RuntimeError("bad"))
    response = await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={"sessionId": session_id, "prompt": [{"type": "text", "text": "hello"}]},
    )
    assert response["error"]["code"] == -32603
    with Session(engine) as session:
        assert list_messages(session, conversation_id) == []
        assert list_runs(session, conversation_id=conversation_id) == []
    await connection.close()


async def test_malformed_permission_response_fails_closed(
    scripted_provider, tmp_path: Path
) -> None:
    scripted_provider.set_script(
        [
            [
                {
                    "id": "call-write",
                    "name": "write_file",
                    "arguments": {"path": "no.txt", "content": "no"},
                }
            ],
            "done",
        ]
    )
    output: list[Any] = []
    connection: ACPConnection

    async def send(message: Any) -> None:
        output.append(message)
        if message.get("method") == "session/request_permission":
            await connection.receive(
                {
                    # Missing jsonrpc must never authorize the action.
                    "id": message["id"],
                    "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}},
                }
            )

    connection = ACPConnection(send, provider_resolver=lambda _model: scripted_provider)
    await _request(
        connection,
        output,
        request_id="init",
        method="initialize",
        params={"protocolVersion": 1, "clientCapabilities": {}},
    )
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    conversation_id = int(session_id.split(":", 1)[1])
    with Session(engine) as session:
        update_conversation(session, conversation_id, permissions={"write_file": "ask"})
    response = await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]},
    )
    assert response["result"]["stopReason"] == "end_turn"
    assert not (tmp_path / "no.txt").exists()
    await connection.close()


async def test_cancel_during_permission_returns_cancelled_not_internal_error(
    scripted_provider, tmp_path: Path
) -> None:
    scripted_provider.set_script(
        [
            [
                {
                    "id": "call-write",
                    "name": "write_file",
                    "arguments": {"path": "cancelled.txt", "content": "no"},
                }
            ]
        ]
    )
    output: list[Any] = []
    connection: ACPConnection

    async def send(message: Any) -> None:
        output.append(message)
        if message.get("method") == "session/request_permission":
            await connection.receive(
                {
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {"sessionId": message["params"]["sessionId"]},
                }
            )

    connection = ACPConnection(send, provider_resolver=lambda _model: scripted_provider)
    await _request(
        connection,
        output,
        request_id="init",
        method="initialize",
        params={"protocolVersion": 1, "clientCapabilities": {}},
    )
    created = await _request(
        connection,
        output,
        request_id="new",
        method="session/new",
        params={"cwd": str(tmp_path), "mcpServers": []},
    )
    session_id = created["result"]["sessionId"]
    conversation_id = int(session_id.split(":", 1)[1])
    with Session(engine) as session:
        update_conversation(session, conversation_id, permissions={"write_file": "ask"})

    response = await _request(
        connection,
        output,
        request_id="prompt",
        method="session/prompt",
        params={"sessionId": session_id, "prompt": [{"type": "text", "text": "write"}]},
    )
    assert response["result"] == {"stopReason": "cancelled"}
    assert not (tmp_path / "cancelled.txt").exists()
    with Session(engine) as session:
        run = list_runs(session, conversation_id=conversation_id)[0]
        assert run.status == "cancelled"
    await connection.close()


@pytest.mark.parametrize(
    "raw",
    [b'{"value":NaN}', b'{"value":Infinity}', (b"[" * 2_000) + (b"]" * 2_000)],
)
def test_stdio_decoder_rejects_non_finite_and_excessively_nested_json(raw: bytes) -> None:
    with pytest.raises((ValueError, RecursionError)):
        decode_message(raw)


@pytest.mark.parametrize("fixture_name", ["zed-v1.json", "reference-v1.json"])
async def test_client_handshake_fixtures_are_schema_valid_and_accepted(
    fixture_name: str, tmp_path: Path
) -> None:
    fixture = json.loads(
        (Path(__file__).parent / "fixtures" / "acp" / fixture_name).read_text(encoding="utf-8")
    )
    output: list[Any] = []

    async def send(message: Any) -> None:
        output.append(message)

    connection = ACPConnection(send)
    for source in fixture["messages"]:
        message = deepcopy(source)
        if message.get("method") == "session/new":
            message["params"]["cwd"] = str(tmp_path)
        _assert_acp_message(message)
        await connection.receive(message)
        await connection.drain()
    assert all("result" in message for message in output)
    assert output[-1]["result"]["sessionId"].startswith("conversation:")
    await connection.close()


def test_canonical_adapter_projects_content_tools_and_plan() -> None:
    canonical = CanonicalEventAdapter(session_id="conversation:1", run_id="run:1")
    acp = ACPEventAdapter()
    token = AgentEvent.token("hello").bind_canonical(canonical)
    tool = AgentEvent.tool_call_start(
        call_id="call-1", name="write_file", arguments={"path": "x"}
    ).bind_canonical(canonical)
    plan = AgentEvent.plan_generated(
        plan_id=1,
        title="Plan",
        steps=[{"title": "Inspect"}, {"title": "Fix"}],
    ).bind_canonical(canonical)
    assert acp.adapt(token.to_canonical_dict())[0]["sessionUpdate"] == "agent_message_chunk"
    assert acp.adapt(tool.to_canonical_dict())[0]["kind"] == "edit"
    plan_update = acp.adapt(plan.to_canonical_dict())[0]
    assert [entry["content"] for entry in plan_update["entries"]] == ["Inspect", "Fix"]


async def test_json_rpc_batch_preserves_request_responses_and_invalid_members() -> None:
    output: list[Any] = []

    async def send(message: Any) -> None:
        output.append(message)

    connection = ACPConnection(send)
    await connection.receive(
        [
            {
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {"protocolVersion": 1, "clientCapabilities": {}},
            },
            {"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "x"}},
            7,
        ]
    )
    await connection.drain()
    assert len(output) == 1
    assert isinstance(output[0], list)
    assert {item.get("id") for item in output[0]} == {None, "init"}
    assert any(item.get("error", {}).get("code") == -32600 for item in output[0])
    await connection.close()


def test_cool_acp_stdio_process_smoke(tmp_path: Path) -> None:
    database = tmp_path / "acp-smoke.db"
    request = {
        "jsonrpc": "2.0",
        "id": "smoke",
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {}},
    }
    env = dict(os.environ)
    env["DATABASE_URL"] = f"sqlite:///{database}"
    env["SCHEDULER_ENABLED"] = "false"
    completed = subprocess.run(
        [sys.executable, "-m", "app.cli", "acp"],
        cwd=Path(__file__).parents[1],
        env=env,
        input=json.dumps(request) + "\n",
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    frames = [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]
    assert len(frames) == 1
    _assert_acp_message(frames[0])
    assert frames[0]["result"]["protocolVersion"] == 1
