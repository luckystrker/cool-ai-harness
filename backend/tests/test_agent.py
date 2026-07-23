"""Tests for the AgentExecutor: tool-calling loop and streaming events."""

from __future__ import annotations

from typing import Any

import pytest

from app.agent import AgentConfig, AgentExecutor
from app.providers import ChatResult, ChatStreamEvent, LLMProvider, Message, Usage


class _ProviderBase(LLMProvider):
    """Minimal scripted provider base for tests; only streaming is used."""

    def __init__(self) -> None:
        self.calls: list[list[Message]] = []
        self.turns: list[Any] = []

    async def chat_completion(self, messages, *, model, tools=None, **kwargs) -> ChatResult:  # type: ignore[override]
        raise NotImplementedError


@pytest.mark.asyncio
async def test_simple_text_response(scripted_provider) -> None:
    """No tool calls: loop should stream tokens and finish on stop."""
    scripted_provider.set_script(["Hello there."])
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))

    events = [e async for e in ex.stream("hi")]

    kinds = [e.kind for e in events]
    assert kinds[0] == "start"
    assert "token" in kinds
    assert kinds[-1] == "finish"
    assert events[-1].payload["reason"] in ("stop", "end_turn")
    text_tokens = "".join(e.payload["text"] for e in events if e.kind == "token")
    assert "Hello there" in text_tokens


@pytest.mark.asyncio
async def test_tool_call_then_answer(scripted_provider) -> None:
    """One tool round-trip: model requests a tool, then gives the final answer."""
    # The first turn: model asks to call a tool that's already registered
    # (write_file), with arguments in JSON-string form (as OpenAI streams them).
    scripted_provider.set_script(
        [
            [
                {
                    "id": "call_1",
                    "name": "write_file",
                    "arguments": {"path": "x.txt", "content": "hi"},
                }
            ],
            "Done writing the file.",
        ]
    )
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))

    events = [e async for e in ex.stream("write hi to x.txt")]

    kinds = [e.kind for e in events]
    # We expect: start, tool_call_start, tool_result, token(s), message, finish
    assert "tool_call_start" in kinds
    assert "tool_result" in kinds

    tool_call = next(e for e in events if e.kind == "tool_call_start")
    assert tool_call.payload["name"] == "write_file"

    tool_result = next(e for e in events if e.kind == "tool_result")
    assert tool_result.payload["result"]["is_error"] is False
    assert "Wrote" in tool_result.payload["result"]["output"]

    # Two LLM round-trips.
    assert len(scripted_provider.calls) == 2
    # History should now contain: user, assistant(tool_calls), tool, assistant(text).
    roles = [m.role for m in ex.history]
    assert roles == ["user", "assistant", "tool", "assistant"]


@pytest.mark.asyncio
async def test_reasoning_streamed_as_thinking(scripted_provider) -> None:
    """A provider reasoning trace is forwarded as `thinking` events and carried
    on the `message` event so it can be persisted."""
    scripted_provider.set_script([
        {"reasoning": "Let me consider the options carefully.", "text": "Answer."}
    ])
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))

    events = [e async for e in ex.stream("hi")]

    thinking_chunks = [e for e in events if e.kind == "thinking"]
    assert thinking_chunks, "expected at least one thinking event"
    reasoning_text = "".join(e.payload["text"] for e in thinking_chunks)
    assert "consider the options" in reasoning_text

    message_ev = next(e for e in events if e.kind == "message")
    assert message_ev.payload["thinking"] is not None
    assert "consider the options" in message_ev.payload["thinking"]


@pytest.mark.asyncio
async def test_tool_result_carries_duration(scripted_provider) -> None:
    """Each tool_result event should record how long the tool took (>= 0)."""
    scripted_provider.set_script(
        [
            [{"id": "call_1", "name": "write_file",
              "arguments": {"path": "x.txt", "content": "hi"}}],
            "Done.",
        ]
    )
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))

    events = [e async for e in ex.stream("write hi")]

    tool_result = next(e for e in events if e.kind == "tool_result")
    metadata = tool_result.payload["result"]["metadata"]
    assert "duration_ms" in metadata
    assert metadata["duration_ms"] >= 0


@pytest.mark.asyncio
async def test_finish_carries_elapsed_ms(scripted_provider) -> None:
    """The terminal finish event should include a non-negative elapsed_ms."""
    scripted_provider.set_script(["ok"])
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))

    events = [e async for e in ex.stream("hi")]

    finish = next(e for e in events if e.kind == "finish")
    assert finish.payload["elapsed_ms"] is not None
    assert finish.payload["elapsed_ms"] >= 0


@pytest.mark.asyncio
async def test_unknown_tool_handled_gracefully(scripted_provider) -> None:
    """An unknown tool name yields an error ToolResult, not an exception."""
    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "does_not_exist", "arguments": {}}],
            "All good.",
        ]
    )
    ex = AgentExecutor(provider=scripted_provider, config=AgentConfig(model="m"))

    events = [e async for e in ex.stream("go")]

    tool_result = next(e for e in events if e.kind == "tool_result")
    assert tool_result.payload["result"]["is_error"] is True
    assert "Unknown tool" in tool_result.payload["result"]["error"]


@pytest.mark.asyncio
async def test_max_iterations_enforced(scripted_provider) -> None:
    """The loop must stop after max_iterations when the model keeps calling tools."""
    # Script: every turn the model wants to call write_file again forever.
    infinite_tool_call = [
        {"id": f"c{i}", "name": "write_file", "arguments": {"path": "f.txt", "content": "x"}}
        for i in range(20)
    ]
    scripted_provider.set_script([[c] for c in infinite_tool_call])

    from app.agent import AgentLimits

    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            limits=AgentLimits(max_iterations=3),
        ),
    )
    events = [e async for e in ex.stream("loop")]

    finish = events[-1]
    assert finish.kind == "finish"
    assert finish.payload["reason"] == "max_iterations"
    assert finish.payload["iterations"] == 3


@pytest.mark.asyncio
async def test_tool_whitelist_filters_tools(scripted_provider, workspace) -> None:
    """tool_names whitelist should limit which ToolSpecs are sent to the model."""
    scripted_provider.set_script(["ok"])
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(model="m", tool_names=["read_file"]),
    )
    specs = ex.available_tools()
    assert [s.name for s in specs] == ["read_file"]


@pytest.mark.asyncio
async def test_tool_call_deltas_as_list(workspace) -> None:
    """Regression: the real OpenAI provider streams delta.tool_calls as a LIST
    of partial tool-call objects (not a single dict). The loop must merge them
    without raising "'list' object has no attribute 'get'".

    Reproduces the failure seen with OpenRouter/DeepSeek where a python_execute
    call produced an error event and no assistant message was persisted.
    """

    class ListDeltaProvider(_ProviderBase):
        name = "list-delta"

        async def chat_completion_stream(self, messages, *, model, tools=None, **kw):
            self.calls.append(list(messages))
            if self.turns:
                turn = self.turns.pop(0)
            else:
                turn = "Done."
            # A plain text turn.
            if isinstance(turn, str):
                for w in turn.split(" "):
                    yield ChatStreamEvent(delta=w + " ")
                yield ChatStreamEvent(finish=True, usage=Usage(1, 1, 2))
                return
            # A tool-call turn streamed OpenAI-style: delta.tool_calls is a LIST,
            # with the arguments JSON arriving in fragments.
            first = True
            for frag in turn["fragments"]:
                payload = [{"index": 0, "function": {"arguments": frag}}]
                if first:
                    payload[0]["id"] = turn["id"]
                    payload[0]["type"] = "function"
                    payload[0]["function"]["name"] = turn["name"]
                    first = False
                yield ChatStreamEvent(tool_call_delta=payload)
            yield ChatStreamEvent(finish=True, finish_reason="tool_calls",
                                  usage=Usage(1, 1, 2))

    provider = ListDeltaProvider()
    provider.turns = [
        {
            "id": "call_42",
            "name": "write_file",
            "fragments": ['{"path":', ' "list.txt",', ' "content": "ok"}'],
        }
    ]

    ex = AgentExecutor(provider=provider, config=AgentConfig(model="m"))
    events = [e async for e in ex.stream("write ok to list.txt")]

    # No error event should be emitted.
    assert not any(e.kind == "error" for e in events), (
        "got error: " + str([e.payload for e in events if e.kind == "error"])
    )
    tool_call = next(e for e in events if e.kind == "tool_call_start")
    assert tool_call.payload["name"] == "write_file"
    assert tool_call.payload["arguments"] == {"path": "list.txt", "content": "ok"}


def test_openai_payload_flat_tool_calls_normalized() -> None:
    """Regression: assistant tool_calls are stored flat ({id,type,name,
    arguments}) but the OpenAI API requires the nested function shape. The
    provider must coerce the flat form when building the request payload, or
    the API replies 400 on the next iteration of the tool loop.
    """
    from app.providers.openai import OpenAIProvider

    provider = OpenAIProvider(base_url="https://example.invalid/v1", api_key="k")
    msg = Message(
        role="assistant",
        content=None,
        tool_calls=[{"id": "c1", "type": "function", "name": "write_file",
                     "arguments": {"path": "x", "content": "hi"}}],
    )
    payload = provider._message_to_payload(msg)
    tc = payload["tool_calls"][0]
    assert tc["id"] == "c1"
    assert tc["type"] == "function"
    # Nested function object with JSON-string arguments — what the API expects.
    assert set(tc["function"].keys()) == {"name", "arguments"}
    assert tc["function"]["name"] == "write_file"
    assert tc["function"]["arguments"] == '{"path": "x", "content": "hi"}'


# --- permission gate tests -------------------------------------------------


@pytest.mark.asyncio
async def test_deny_policy_blocks_tool_and_continues(scripted_provider) -> None:
    """A 'deny' policy yields an error tool_result and the loop keeps going."""
    from app.agent.permissions import PermissionsConfig

    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "write_file",
              "arguments": {"path": "blocked.txt", "content": "x"}}],
            "Recovered.",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            permissions=PermissionsConfig(tools={"write_file": "deny"}),
        ),
    )
    events = [e async for e in ex.stream("go")]

    tool_result = next(e for e in events if e.kind == "tool_result")
    assert tool_result.payload["result"]["is_error"] is True
    assert "Permission denied" in tool_result.payload["result"]["error"]
    # No file was actually written.
    from app.tools.context import get_run_context

    written = list(get_run_context().workdir.glob("blocked.txt"))
    assert written == []
    # The loop continued to a second LLM round-trip.
    assert len(scripted_provider.calls) == 2


@pytest.mark.asyncio
async def test_ask_with_auto_approve_runs_tool(scripted_provider, workspace) -> None:
    """auto_approve=True turns 'ask' into 'allow' without prompting."""
    from app.agent.permissions import PermissionsConfig

    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "write_file",
              "arguments": {"path": "ok.txt", "content": "hi"}}],
            "Done.",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            permissions=PermissionsConfig(tools={"write_file": "ask"}),
            auto_approve=True,
        ),
    )
    events = [e async for e in ex.stream("go")]

    # No approval request should be emitted when auto_approve is on.
    assert not any(e.kind == "tool_approval_request" for e in events)
    tool_result = next(e for e in events if e.kind == "tool_result")
    assert tool_result.payload["result"]["is_error"] is False
    assert (workspace / "ok.txt").read_text() == "hi"


@pytest.mark.asyncio
async def test_ask_times_out_into_deny(scripted_provider, monkeypatch) -> None:
    """When nobody resolves the approval, the loop auto-denies after a timeout."""
    import app.agent.executor as executor_module
    from app.agent.permissions import PermissionsConfig

    # Shrink the timeout so the test doesn't wait 5 minutes.
    monkeypatch.setattr(executor_module, "DEFAULT_APPROVAL_TIMEOUT_S", 0.1)

    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "write_file",
              "arguments": {"path": "no.txt", "content": "x"}}],
            "Recovered.",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            permissions=PermissionsConfig(tools={"write_file": "ask"}),
        ),
    )
    events = [e async for e in ex.stream("go")]

    # An approval request must have been emitted.
    assert any(e.kind == "tool_approval_request" for e in events)
    tool_result = next(e for e in events if e.kind == "tool_result")
    assert tool_result.payload["result"]["is_error"] is True
    assert "denied" in tool_result.payload["result"]["error"].lower()


@pytest.mark.asyncio
async def test_allow_policy_runs_without_approval(scripted_provider, workspace) -> None:
    """An explicit 'allow' runs the tool and never asks for approval."""
    from app.agent.permissions import PermissionsConfig

    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "write_file",
              "arguments": {"path": "a.txt", "content": "hi"}}],
            "Done.",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            permissions=PermissionsConfig(tools={"write_file": "allow"}),
        ),
    )
    events = [e async for e in ex.stream("go")]
    assert not any(e.kind == "tool_approval_request" for e in events)
    assert (workspace / "a.txt").read_text() == "hi"


@pytest.mark.asyncio
async def test_wildcard_deny_blocks_all(scripted_provider) -> None:
    """A '*' deny blocks every tool (including dangerous ones)."""
    from app.agent.permissions import PermissionsConfig

    scripted_provider.set_script(
        [
            [{"id": "c1", "name": "list_files", "arguments": {"path": "."}}],
            "Recovered.",
        ]
    )
    ex = AgentExecutor(
        provider=scripted_provider,
        config=AgentConfig(
            model="m",
            permissions=PermissionsConfig(tools={"*": "deny"}),
        ),
    )
    events = [e async for e in ex.stream("go")]
    tool_result = next(e for e in events if e.kind == "tool_result")
    assert tool_result.payload["result"]["is_error"] is True
