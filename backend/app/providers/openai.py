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
    ModelInfo,
    ToolSpec,
    Usage,
)
from app.providers.pricing import estimate_cost_usd, get_model_pricing

log = get_logger(__name__)


class OpenAIProvider(LLMProvider):
    name = "openai"

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str,
        default_model: str | None = None,
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
    def _parse_usage(
        raw_usage: dict[str, Any] | None, *, model: str | None = None
    ) -> Usage | None:
        if not raw_usage:
            return None
        prompt = raw_usage.get("prompt_tokens", 0)
        completion = raw_usage.get("completion_tokens", 0)
        total = raw_usage.get("total_tokens", 0)
        cost = (
            estimate_cost_usd(model, prompt, completion) if model else None
        )
        return Usage(
            prompt_tokens=prompt,
            completion_tokens=completion,
            total_tokens=total,
            cost_usd=cost,
        )

    # ---- model discovery ----

    async def list_models(self) -> list[ModelInfo]:
        """List models served by this backend via ``GET {base_url}/models``.

        Most OpenAI-compatible backends answer this; the response is the
        OpenAI shape ``{"data": [{"id": ...}, ...]}``. Some (OpenRouter, Groq,
        Ollama, LM Studio) enrich each entry with a context length under
        ``context_length`` / ``context_window``. Prices are filled in from the
        local pricing table where the id matches a known entry.
        """
        url = f"{self.base_url}/models"
        async with httpx.AsyncClient(timeout=20.0, transport=self._transport) as client:
            resp = await client.get(url, headers=self._headers())
            resp.raise_for_status()
            payload = resp.json()

        items = payload.get("data") if isinstance(payload, dict) else payload
        if not isinstance(items, list):
            return []

        out: list[ModelInfo] = []
        for item in items:
            if not isinstance(item, dict):
                continue
            mid = item.get("id")
            if not isinstance(mid, str) or not mid:
                continue
            ctx = _extract_context_window(item)
            prices = get_model_pricing(mid) or {}
            out.append(
                ModelInfo(
                    id=mid,
                    context_window=ctx,
                    prompt_price=prices.get("prompt"),
                    completion_price=prices.get("completion"),
                )
            )
        out.sort(key=lambda m: m.id)
        return out

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
            usage=self._parse_usage(data.get("usage"), model=model),
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
                    event.usage = self._parse_usage(chunk["usage"], model=model)
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


def _extract_context_window(item: dict[str, Any]) -> int | None:
    """Pull a context length out of a ``/models`` entry, across vendors.

    Field name varies: OpenRouter uses ``context_length``, Ollama ``context_window``
    (sometimes nested), Groq ``context_window``. Returns None when absent.
    """
    for key in ("context_length", "context_window", "max_context", "n_ctx"):
        val = item.get(key)
        if isinstance(val, int) and val > 0:
            return val
    # LM Studio nests it under {"top_provider": {"context_length": ...}}.
    top = item.get("top_provider")
    if isinstance(top, dict):
        for key in ("context_length", "context_window"):
            val = top.get(key)
            if isinstance(val, int) and val > 0:
                return val
    return None
