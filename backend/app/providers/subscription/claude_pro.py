"""Claude Pro/Max subscription adapter (EXPERIMENTAL).

Provides access to Claude models via a Claude Pro/Max subscription session
rather than an Anthropic API key. Uses the internal claude.ai API endpoints.

WARNING: This is experimental and unofficial. It may:
- Break at any time when Anthropic changes their internal API
- Violate Anthropic's Terms of Service
- Have different rate limits than the API (governed by subscription tier)

Authentication:
    Requires a session cookie from an authenticated claude.ai browser session.
    Extract the ``sessionKey`` cookie value and pass it as ``session_token``.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import Any

import httpx

from app.providers.base import (
    ChatResult,
    ChatStreamEvent,
    Message,
    ModelInfo,
    ToolSpec,
    Usage,
    message_text,
)
from app.providers.subscription.base import SubscriptionProvider

# Default models available on Claude Pro/Max subscriptions.
CLAUDE_PRO_MODELS = [
    "claude-sonnet-4-20250514",
    "claude-3-5-haiku-20241022",
]

CLAUDE_MAX_MODELS = [
    "claude-sonnet-4-20250514",
    "claude-3-5-haiku-20241022",
    "claude-3-opus-20240229",
]


class ClaudeProProvider(SubscriptionProvider):
    """Experimental adapter for Claude Pro/Max subscriptions.

    Usage::

        provider = ClaudeProProvider(
            session_token="sk-ant-...",  # sessionKey cookie value
            tier="max",  # "pro" or "max"
        )
        result = await provider.chat_completion(messages, model="claude-sonnet-4-20250514")
    """

    name = "subscription/claude_pro"

    def __init__(
        self,
        *,
        session_token: str,
        tier: str = "pro",
        default_model: str | None = None,
        timeout_s: float = 120.0,
    ) -> None:
        super().__init__(
            session_token=session_token,
            base_url="https://claude.ai/api",
            default_model=default_model or CLAUDE_PRO_MODELS[0],
            timeout_s=timeout_s,
        )
        self._tier = tier

    def _auth_headers(self) -> dict[str, str]:
        """Claude.ai uses cookie-based session auth."""
        return {
            "Cookie": f"sessionKey={self._session_token}",
            "Content-Type": "application/json",
            "Accept": "text/event-stream",
        }

    def _build_request_payload(
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None,
        temperature: float,
        max_tokens: int | None,
        stream: bool,
        **kwargs: Any,
    ) -> dict[str, Any]:
        """Build claude.ai internal API payload.

        Note: The internal API format differs from the official Anthropic API.
        This is a best-effort mapping that may need updates.
        """
        # Convert messages to claude.ai format.
        prompt_messages = []
        system_text = ""
        for msg in messages:
            if msg.role == "system":
                system_text += message_text(msg.content) + "\n"
            else:
                prompt_messages.append(
                    {
                        "role": msg.role,
                        "content": message_text(msg.content),
                    }
                )

        payload: dict[str, Any] = {
            "model": model,
            "messages": prompt_messages,
            "temperature": temperature,
            "stream": stream,
        }
        if system_text:
            payload["system"] = system_text.strip()
        if max_tokens:
            payload["max_tokens"] = max_tokens
        # Note: tool calling may not be supported via subscription endpoints.
        return payload

    def _parse_response(self, data: dict[str, Any]) -> ChatResult:
        """Parse claude.ai response format."""
        content = ""
        if "completion" in data:
            content = data["completion"]
        elif "content" in data:
            # Array of content blocks.
            blocks = data["content"]
            if isinstance(blocks, list):
                content = "".join(b.get("text", "") for b in blocks if b.get("type") == "text")
            elif isinstance(blocks, str):
                content = blocks

        usage = None
        if "usage" in data:
            u = data["usage"]
            usage = Usage(
                prompt_tokens=u.get("input_tokens", 0),
                completion_tokens=u.get("output_tokens", 0),
                total_tokens=u.get("input_tokens", 0) + u.get("output_tokens", 0),
            )

        return ChatResult(
            content=content or None,
            usage=usage,
            finish_reason=data.get("stop_reason", "end_turn"),
        )

    async def _parse_stream(self, response: httpx.Response) -> AsyncIterator[ChatStreamEvent]:
        """Parse SSE stream from claude.ai."""
        async for line in response.aiter_lines():
            if not line.startswith("data: "):
                continue
            data_str = line[6:]
            if data_str == "[DONE]":
                yield ChatStreamEvent(finish=True, finish_reason="end_turn")
                return
            try:
                data = json.loads(data_str)
            except json.JSONDecodeError:
                continue

            event_type = data.get("type", "")
            if event_type == "content_block_delta":
                delta = data.get("delta", {})
                text = delta.get("text", "")
                if text:
                    yield ChatStreamEvent(delta=text)
            elif event_type == "message_stop":
                yield ChatStreamEvent(finish=True, finish_reason="end_turn")
                return

    async def list_models(self) -> list[ModelInfo]:
        """Return models available for the subscription tier."""
        models = CLAUDE_MAX_MODELS if self._tier == "max" else CLAUDE_PRO_MODELS
        return [ModelInfo(id=m) for m in models]
