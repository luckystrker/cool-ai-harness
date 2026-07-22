"""OpenAI-compatible LLM provider.

Works with OpenAI, OpenRouter, DeepSeek, Groq, Together, and local Ollama /
LM Studio / vLLM — anything that speaks the OpenAI Chat Completions API.
A custom `base_url` selects the backend; an `api_key` authorizes it.
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


class OpenAIProvider(LLMProvider):
    name = "openai"

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str,
        default_model: str = "gpt-4o-mini",
        timeout: float = 120.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.default_model = default_model
        self.timeout = timeout

    # ---- request shaping ----

    def _headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

    @staticmethod
    def _message_to_payload(m: Message) -> dict[str, Any]:
        out: dict[str, Any] = {"role": m.role}
        if m.content is not None:
            out["content"] = m.content
        if m.tool_calls is not None:
            # The harness stores tool_calls in a flat canonical shape:
            #   {"id", "type", "name", "arguments": <dict>}
            # OpenAI's Chat Completions API requires the nested shape:
            #   {"id", "type", "function": {"name", "arguments": <json-string>}}
            # Normalize both incoming shapes so we never send a 400 back to the
            # provider when replaying an assistant's tool calls in a later turn.
            out["tool_calls"] = [_to_openai_tool_call(tc) for tc in m.tool_calls]
        if m.tool_call_id is not None:
            out["tool_call_id"] = m.tool_call_id
        if m.name is not None:
            out["name"] = m.name
        return out

    @staticmethod
    def _tools_to_payload(tools: list[ToolSpec] | None) -> list[dict[str, Any]] | None:
        if not tools:
            return None
        return [
            {"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}
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
        payload: dict[str, Any] = {
            "model": model or self.default_model,
            "messages": [self._message_to_payload(m) for m in messages],
            "temperature": temperature,
            "stream": stream,
        }
        if max_tokens is not None:
            payload["max_tokens"] = max_tokens
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
        return Usage(
            prompt_tokens=raw_usage.get("prompt_tokens", 0),
            completion_tokens=raw_usage.get("completion_tokens", 0),
            total_tokens=raw_usage.get("total_tokens", 0),
        )

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
        url = f"{self.base_url}/chat/completions"
        async with httpx.AsyncClient(timeout=self.timeout) as client:
            resp = await client.post(url, headers=self._headers(), json=payload)
            resp.raise_for_status()
            data = resp.json()

        choice = data["choices"][0]["message"]
        return ChatResult(
            content=choice.get("content"),
            tool_calls=choice.get("tool_calls"),
            usage=self._parse_usage(data.get("usage")),
            finish_reason=data["choices"][0].get("finish_reason"),
            # Reasoning models (DeepSeek-R1 via OpenRouter, etc.) put the
            # chain-of-thought under `reasoning_content` (DeepSeek) or
            # `reasoning` (OpenRouter). Pick whichever is present.
            reasoning=choice.get("reasoning_content") or choice.get("reasoning"),
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
        # Ask the server to send usage on the final chunk when supported.
        payload.setdefault("stream_options", {"include_usage": True})

        url = f"{self.base_url}/chat/completions"
        async with httpx.AsyncClient(timeout=self.timeout) as client, client.stream(
            "POST", url, headers=self._headers(), json=payload
        ) as resp:
            resp.raise_for_status()
            async for line in resp.aiter_lines():
                if not line or not line.startswith("data:"):
                    continue
                data_str = line[len("data:") :].strip()
                if data_str == "[DONE]":
                    yield ChatStreamEvent(finish=True)
                    return
                try:
                    chunk = json.loads(data_str)
                except json.JSONDecodeError:
                    log.warning("providers.openai.bad_chunk", line=data_str)
                    continue

                choices = chunk.get("choices") or []
                delta_obj = choices[0].get("delta", {}) if choices else {}
                finish_reason = choices[0].get("finish_reason") if choices else None

                event = ChatStreamEvent(
                    delta=delta_obj.get("content") or "",
                    reasoning=delta_obj.get("reasoning_content")
                    or delta_obj.get("reasoning")
                    or "",
                    tool_call_delta=(delta_obj.get("tool_calls") or None),
                    finish_reason=finish_reason,
                    finish=finish_reason is not None,
                )
                if chunk.get("usage"):
                    event.usage = self._parse_usage(chunk["usage"])
                yield event


def _to_openai_tool_call(tc: dict[str, Any]) -> dict[str, Any]:
    """Coerce a stored tool call into OpenAI's wire shape.

    Accepts both the harness's flat canonical form (``{id, type, name,
    arguments}``) and an already-OpenAI-shaped call (``{id, type,
    function: {name, arguments}}``). ``arguments`` is serialized to a JSON
    string, as the API requires.
    """
    fn = tc.get("function")
    if isinstance(fn, dict):
        name = fn.get("name", "")
        args = fn.get("arguments", {})
    else:
        name = tc.get("name", "")
        args = tc.get("arguments", {})
    args_str = args if isinstance(args, str) else json.dumps(args or {}, ensure_ascii=False)
    return {
        "id": tc.get("id"),
        "type": tc.get("type", "function"),
        "function": {"name": name, "arguments": args_str},
    }
