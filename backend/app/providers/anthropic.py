"""Native Anthropic (Claude) LLM provider.

Talks to Anthropic's Messages API (https://api.anthropic.com/v1/messages)
directly, rather than through an OpenAI-compatible proxy. Differences from the
OpenAI wire format that this provider papers over:

  * Auth via ``x-api-key`` + ``anthropic-version`` headers (not Bearer token).
  * The system prompt is a top-level ``system`` field, not a chat message.
  * ``max_tokens`` is required.
  * Tools are declared as ``{name, description, input_schema}`` (not
    ``function.parameters``), and tool calls/results are ``tool_use`` /
    ``tool_result`` content *blocks* rather than a separate message shape.
  * Streaming uses typed SSE events (``content_block_delta`` etc.), not
    OpenAI's ``data: {choices:[...]}`` chunks.

Outgoing ``tool_calls`` are returned in OpenAI's nested shape
(``{id, type, function: {name, arguments: <json-str>}}``) so the agent loop's
existing delta-merging / canonicalization code — which already speaks that
shape — works unchanged.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import Any

import httpx

from app.core.logging import get_logger
from app.providers.base import (
    ChatResult,
    ChatStreamEvent,
    LLMProvider,
    Message,
    ToolSpec,
    Usage,
)

log = get_logger(__name__)

# Anthropic requires max_tokens on every request; pick a sane default when the
# caller (agent loop) doesn't specify one.
_DEFAULT_MAX_TOKENS = 4096
_API_VERSION = "2023-06-01"


class AnthropicProvider(LLMProvider):
    name = "anthropic"

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str,
        default_model: str = "claude-3-5-sonnet-latest",
        timeout: float = 120.0,
        transport: Any | None = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.default_model = default_model
        self.timeout = timeout
        # Optional injected httpx transport — used by tests to avoid real
        # network calls (httpx.MockTransport). None in production.
        self._transport = transport

    # ---- request shaping ----

    def _headers(self) -> dict[str, str]:
        return {
            "x-api-key": self.api_key,
            "anthropic-version": _API_VERSION,
            "content-type": "application/json",
        }

    @staticmethod
    def _extract_system(messages: list[Message]) -> tuple[str | None, list[Message]]:
        """Pull system messages out of the conversation.

        Anthropic's API takes the system prompt as a top-level field, not as a
        chat message. Concatenate every system message into one string; the
        remaining messages are returned verbatim.
        """
        parts: list[str] = []
        rest: list[Message] = []
        for m in messages:
            if m.role == "system":
                if m.content:
                    parts.append(m.content)
            else:
                rest.append(m)
        system = "\n\n".join(parts) if parts else None
        return system, rest

    @staticmethod
    def _message_to_blocks(m: Message) -> tuple[str, list[dict[str, Any]]]:
        """Convert one harness Message into Anthropic's (role, content-blocks).

        Returns the role plus a list of content blocks. The harness canonical
        shape is mapped onto Anthropic's block types:

          * assistant tool_calls  -> ``tool_use`` blocks
          * tool-result messages  -> a single ``tool_result`` block (role=user)

        Plain text stays as a string block.
        """
        if m.role == "tool":
            # The harness carries a tool result as Message(role="tool",
            # content=<output>, tool_call_id=<id>, name=<name>). Anthropic
            # wants this as a user-role message with a tool_result block that
            # references the tool_use id it produced earlier.
            block: dict[str, Any] = {
                "type": "tool_result",
                "tool_use_id": m.tool_call_id,
                "content": m.content,
            }
            return "user", [block]

        if m.role == "assistant" and m.tool_calls:
            blocks: list[dict[str, Any]] = []
            if m.content:
                blocks.append({"type": "text", "text": m.content})
            for tc in m.tool_calls:
                fn = tc.get("function")
                if isinstance(fn, dict):
                    name = fn.get("name", "")
                    raw_args = fn.get("arguments", {})
                else:
                    name = tc.get("name", "")
                    raw_args = tc.get("arguments", {})
                # Anthropic wants the input as a JSON object, not a string.
                if isinstance(raw_args, str):
                    try:
                        parsed = json.loads(raw_args) if raw_args else {}
                    except json.JSONDecodeError:
                        parsed = {"_raw": raw_args}
                else:
                    parsed = raw_args or {}
                blocks.append(
                    {
                        "type": "tool_use",
                        "id": tc.get("id"),
                        "name": name,
                        "input": parsed,
                    }
                )
            return "assistant", blocks

        # Plain user/assistant text.
        return m.role, [{"type": "text", "text": m.content or ""}]

    @staticmethod
    def _tools_to_payload(tools: list[ToolSpec] | None) -> list[dict[str, Any]] | None:
        if not tools:
            return None
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            }
            for t in tools
        ]

    def _build_payload(
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None,
        temperature: float,
        max_tokens: int | None,
        stream: bool,
        extra: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        system, convo = self._extract_system(messages)
        payload: dict[str, Any] = {
            "model": model or self.default_model,
            "max_tokens": max_tokens or _DEFAULT_MAX_TOKENS,
            "temperature": temperature,
            "stream": stream,
            # Anthropic takes messages as [{role, content: <blocks>}].
            "messages": [
                {"role": role, "content": blocks}
                for role, blocks in (self._message_to_blocks(m) for m in convo)
            ],
        }
        if system is not None:
            payload["system"] = system
        fns = self._tools_to_payload(tools)
        if fns:
            payload["tools"] = fns
        if extra:
            payload.update(extra)
        return payload

    @staticmethod
    def _parse_usage(raw_usage: dict[str, Any] | None) -> Usage | None:
        if not raw_usage:
            return None
        prompt = raw_usage.get("input_tokens", 0)
        completion = raw_usage.get("output_tokens", 0)
        return Usage(
            prompt_tokens=prompt,
            completion_tokens=completion,
            total_tokens=prompt + completion,
        )

    @staticmethod
    def _blocks_to_result(
        content_blocks: list[dict[str, Any]],
    ) -> tuple[str | None, list[dict[str, Any]] | None, str | None]:
        """Flatten Anthropic response content blocks into (text, tool_calls, thinking).

        ``tool_calls`` is emitted in OpenAI's nested shape so the agent loop can
        consume it unchanged.
        """
        text_parts: list[str] = []
        thinking_parts: list[str] = []
        tool_calls: list[dict[str, Any]] = []
        for block in content_blocks:
            btype = block.get("type")
            if btype == "text":
                text_parts.append(block.get("text", ""))
            elif btype == "tool_use":
                tool_calls.append(
                    {
                        "id": block.get("id"),
                        "type": "function",
                        "function": {
                            "name": block.get("name", ""),
                            "arguments": json.dumps(
                                block.get("input") or {}, ensure_ascii=False
                            ),
                        },
                    }
                )
            elif btype == "thinking":
                thinking_parts.append(block.get("thinking", ""))
        text = "".join(text_parts) if text_parts else None
        reasoning = "".join(thinking_parts) if thinking_parts else None
        return text, (tool_calls or None), reasoning

    # ---- non-streaming ----

    async def chat_completion(
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        **kwargs: Any,
    ) -> ChatResult:
        payload = self._build_payload(
            messages,
            model=model,
            tools=tools,
            temperature=temperature,
            max_tokens=max_tokens,
            stream=False,
            extra=kwargs.get("extra"),
        )
        url = f"{self.base_url}/v1/messages"
        async with httpx.AsyncClient(timeout=self.timeout, transport=self._transport) as client:
            resp = await client.post(url, headers=self._headers(), json=payload)
            resp.raise_for_status()
            data = resp.json()

        blocks = data.get("content") or []
        text, tool_calls, reasoning = self._blocks_to_result(blocks)
        return ChatResult(
            content=text,
            tool_calls=tool_calls,
            usage=self._parse_usage(data.get("usage")),
            finish_reason=data.get("stop_reason"),
            reasoning=reasoning,
            raw=data,
        )

    # ---- streaming ----

    async def chat_completion_stream(
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        **kwargs: Any,
    ) -> AsyncIterator[ChatStreamEvent]:
        payload = self._build_payload(
            messages,
            model=model,
            tools=tools,
            temperature=temperature,
            max_tokens=max_tokens,
            stream=True,
            extra=kwargs.get("extra"),
        )
        url = f"{self.base_url}/v1/messages"

        # Track the active content block so tool-input deltas can be attached to
        # the right tool_use id (Anthropic streams input as input_json_delta).
        current_block_index: int | None = None
        # block index -> {"id": tool_use_id, "name": name}
        tool_blocks: dict[int, dict[str, Any]] = {}

        async with httpx.AsyncClient(timeout=self.timeout, transport=self._transport) as client, client.stream(
            "POST", url, headers=self._headers(), json=payload
        ) as resp:
            resp.raise_for_status()
            async for line in resp.aiter_lines():
                if not line or not line.startswith("data:"):
                    continue
                data_str = line[len("data:") :].strip()
                if not data_str:
                    continue
                try:
                    evt = json.loads(data_str)
                except json.JSONDecodeError:
                    log.warning("providers.anthropic.bad_chunk", line=data_str)
                    continue

                etype = evt.get("type")

                if etype == "content_block_start":
                    current_block_index = evt.get("index")
                    block = evt.get("content_block", {}) or {}
                    if block.get("type") == "tool_use":
                        tool_blocks[current_block_index] = {
                            "id": block.get("id"),
                            "name": block.get("name", ""),
                        }

                elif etype == "content_block_delta":
                    delta = evt.get("delta", {}) or {}
                    dtype = delta.get("type")
                    if dtype == "text_delta":
                        yield ChatStreamEvent(delta=delta.get("text", ""))
                    elif dtype == "thinking_delta":
                        yield ChatStreamEvent(reasoning=delta.get("thinking", ""))
                    elif dtype == "input_json_delta":
                        idx = evt.get("index", current_block_index)
                        meta = tool_blocks.get(idx, {})
                        # Emit OpenAI-shaped partial tool_call fragments so the
                        # agent loop's delta-merging builds the final call.
                        yield ChatStreamEvent(
                            tool_call_delta=[
                                {
                                    "index": idx or 0,
                                    "id": meta.get("id"),
                                    "type": "function",
                                    "function": {
                                        "name": meta.get("name", ""),
                                        "arguments": delta.get("partial_json", ""),
                                    },
                                }
                            ]
                        )

                elif etype == "content_block_stop":
                    current_block_index = None

                elif etype == "message_delta":
                    # Carries the final usage + stop_reason on the terminal delta.
                    msg_delta = evt.get("delta", {}) or {}
                    usage = self._parse_usage(evt.get("usage"))
                    finish_reason = msg_delta.get("stop_reason")
                    yield ChatStreamEvent(
                        finish=True,
                        finish_reason=finish_reason,
                        usage=usage,
                    )

                elif etype == "message_stop":
                    # Belt-and-braces terminal event in case message_delta
                    # didn't carry the finish (some proxy paths strip it).
                    return
