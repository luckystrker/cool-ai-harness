"""Tests for the native AnthropicProvider.

Covers the request-shaping (system hoisting, content-block mapping, tool
schema) and the response/stream parsing — all without real network calls. HTTP
is exercised via httpx.MockTransport injected through the provider's
``transport=`` constructor argument.
"""

from __future__ import annotations

import json
from typing import Any

import httpx

from app.providers.anthropic import AnthropicProvider
from app.providers.base import Message, ToolSpec


def _sse_lines(events: list[dict[str, Any]]) -> bytes:
    """Serialize Anthropic SSE events into the wire byte stream."""
    out: list[str] = []
    for evt in events:
        out.append("event: " + evt.get("type", ""))
        out.append("data: " + json.dumps(evt))
        out.append("")
        out.append("")
    return ("\n".join(out)).encode()


def _provider(transport: httpx.MockTransport) -> AnthropicProvider:
    return AnthropicProvider(
        base_url="https://api.anthropic.com",
        api_key="sk-ant-test",
        default_model="claude-3-5-sonnet-latest",
        transport=transport,
    )


# --- request shaping (pure, no HTTP) -----------------------------------------


def test_system_message_hoisted_into_top_level_field() -> None:
    """A system message must leave the messages list and become payload.system."""
    p = AnthropicProvider(base_url="https://api.anthropic.com", api_key="k")
    payload = p._build_payload(
        [
            Message(role="system", content="You are helpful."),
            Message(role="user", content="Hi"),
        ],
        model="claude",
        tools=None,
        temperature=0.5,
        max_tokens=None,
        stream=False,
    )
    assert payload["system"] == "You are helpful."
    # No system entry remains inside messages.
    assert [m["role"] for m in payload["messages"]] == ["user"]
    # max_tokens is required by Anthropic — a default fills in when None.
    assert payload["max_tokens"] == 4096


def test_assistant_tool_calls_mapped_to_tool_use_blocks() -> None:
    """Stored (canonical / nested) tool_calls become Anthropic tool_use blocks,
    with input as a JSON object (not a string)."""
    p = AnthropicProvider(base_url="https://api.anthropic.com", api_key="k")
    role, blocks = p._message_to_blocks(
        Message(
            role="assistant",
            content=None,
            tool_calls=[
                # Canonical flat shape (how the harness stores it).
                {"id": "c1", "type": "function", "name": "write_file",
                 "arguments": {"path": "x", "content": "hi"}}
            ],
        )
    )
    assert role == "assistant"
    tu = next(b for b in blocks if b["type"] == "tool_use")
    assert tu["id"] == "c1"
    assert tu["name"] == "write_file"
    # input is an object, not a JSON string.
    assert tu["input"] == {"path": "x", "content": "hi"}


def test_assistant_nested_tool_call_arguments_string_parsed() -> None:
    """When replaying an OpenAI-shaped call, arguments arrive as a JSON string
    and must be parsed back to an object for Anthropic."""
    p = AnthropicProvider(base_url="https://api.anthropic.com", api_key="k")
    _, blocks = p._message_to_blocks(
        Message(
            role="assistant",
            tool_calls=[
                {"id": "c2", "type": "function",
                 "function": {"name": "read_file", "arguments": '{"path": "a.txt"}'}}
            ],
        )
    )
    tu = next(b for b in blocks if b["type"] == "tool_use")
    assert tu["input"] == {"path": "a.txt"}


def test_tool_result_message_mapped_to_tool_result_block_as_user_role() -> None:
    """A role='tool' message becomes a user-role tool_result block referencing
    the original tool_use id."""
    p = AnthropicProvider(base_url="https://api.anthropic.com", api_key="k")
    role, blocks = p._message_to_blocks(
        Message(role="tool", content="file contents", tool_call_id="c1", name="read_file")
    )
    assert role == "user"
    assert blocks[0]["type"] == "tool_result"
    assert blocks[0]["tool_use_id"] == "c1"
    assert blocks[0]["content"] == "file contents"


def test_tools_use_input_schema_not_function_parameters() -> None:
    p = AnthropicProvider(base_url="https://api.anthropic.com", api_key="k")
    schema = {"type": "object", "properties": {"q": {"type": "string"}}}
    out = p._tools_to_payload([ToolSpec(name="search", description="d", parameters=schema)])
    assert out is not None
    assert out[0]["input_schema"] == schema
    assert "function" not in out[0]


def test_headers_use_anthropic_auth() -> None:
    p = AnthropicProvider(base_url="https://api.anthropic.com", api_key="sk-ant-x")
    h = p._headers()
    assert h["x-api-key"] == "sk-ant-x"
    assert h["anthropic-version"] == "2023-06-01"
    assert "Authorization" not in h


# --- non-streaming parsing ---------------------------------------------------


async def test_chat_completion_parses_text_and_usage() -> None:
    captured: dict[str, Any] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["url"] = str(request.url)
        captured["headers"] = dict(request.headers)
        captured["json"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "Hello!"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 12, "output_tokens": 3},
            },
        )

    p = _provider(httpx.MockTransport(handler))
    result = await p.chat_completion(
        [Message(role="user", content="hi")], model="claude"
    )

    assert result.content == "Hello!"
    assert result.finish_reason == "end_turn"
    assert result.usage is not None
    assert result.usage.prompt_tokens == 12
    assert result.usage.completion_tokens == 3
    assert result.usage.total_tokens == 15
    # Posted to the Messages endpoint with the Anthropic auth header.
    assert captured["url"].endswith("/v1/messages")
    assert captured["headers"]["x-api-key"] == "sk-ant-test"


async def test_chat_completion_parses_tool_use_blocks() -> None:
    """A tool_use block becomes an OpenAI-shaped tool_call in the result."""
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "tu_1", "name": "search",
                     "input": {"q": "cats"}},
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 5, "output_tokens": 10},
            },
        )

    p = _provider(httpx.MockTransport(handler))
    result = await p.chat_completion([Message(role="user", content="go")], model="claude")

    assert result.content == "Let me check."
    assert result.tool_calls is not None
    tc = result.tool_calls[0]
    # OpenAI nested shape — what the agent loop canonicalizes.
    assert tc["id"] == "tu_1"
    assert tc["type"] == "function"
    assert tc["function"]["name"] == "search"
    assert json.loads(tc["function"]["arguments"]) == {"q": "cats"}


# --- streaming parsing -------------------------------------------------------


async def test_stream_emits_text_tokens_and_finish_usage() -> None:
    events = [
        {"type": "message_start", "message": {"usage": {"input_tokens": 7}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"},
         "usage": {"input_tokens": 7, "output_tokens": 2}},
        {"type": "message_stop"},
    ]

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=_sse_lines(events),
                              headers={"content-type": "text/event-stream"})

    p = _provider(httpx.MockTransport(handler))
    out = [e async for e in p.chat_completion_stream([Message(role="user", content="hi")], model="claude")]

    text = "".join(e.delta for e in out)
    assert text == "Hello"
    finish = [e for e in out if e.finish]
    assert finish, "expected a terminal finish event"
    assert finish[-1].usage is not None
    assert finish[-1].usage.completion_tokens == 2
    assert finish[-1].finish_reason == "end_turn"


async def test_stream_tool_input_json_delta_becomes_tool_call_delta() -> None:
    """Anthropic streams tool input as input_json_delta fragments; they must be
    surfaced as OpenAI-shaped tool_call_delta fragments for the agent loop."""
    events = [
        {"type": "content_block_start", "index": 0,
         "content_block": {"type": "tool_use", "id": "tu_9", "name": "write_file"}},
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "input_json_delta", "partial_json": '{"path":'}},
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "input_json_delta", "partial_json": ' "a.txt"}'}},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use"},
         "usage": {"input_tokens": 1, "output_tokens": 1}},
        {"type": "message_stop"},
    ]

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=_sse_lines(events),
                              headers={"content-type": "text/event-stream"})

    p = _provider(httpx.MockTransport(handler))
    out = [e async for e in p.chat_completion_stream([Message(role="user", content="go")], model="claude")]

    deltas = [e for e in out if e.tool_call_delta is not None]
    assert len(deltas) == 2
    # Each fragment is an OpenAI-shaped list with index/id/function.
    first = deltas[0].tool_call_delta[0]
    assert first["id"] == "tu_9"
    assert first["function"]["name"] == "write_file"
    assert first["function"]["arguments"] == '{"path":'
    # Concatenated fragments reconstruct the JSON the model intended.
    joined = "".join(d.tool_call_delta[0]["function"]["arguments"] for d in deltas)
    assert json.loads(joined) == {"path": "a.txt"}


async def test_stream_thinking_delta_mapped_to_reasoning() -> None:
    events = [
        {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking"}},
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "thinking_delta", "thinking": "Hmm"}},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {}},
        {"type": "message_stop"},
    ]

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=_sse_lines(events),
                              headers={"content-type": "text/event-stream"})

    p = _provider(httpx.MockTransport(handler))
    out = [e async for e in p.chat_completion_stream([Message(role="user", content="hi")], model="claude")]
    reasoning = "".join(e.reasoning for e in out)
    assert reasoning == "Hmm"
