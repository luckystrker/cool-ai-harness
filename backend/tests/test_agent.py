"""Tests for the AgentExecutor: tool-calling loop and streaming events."""

from __future__ import annotations

import pytest

from app.agent import AgentConfig, AgentExecutor


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
