"""ChatGPT Plus subscription adapter (EXPERIMENTAL).

Provides access to GPT models via a ChatGPT Plus subscription session
rather than an OpenAI API key. Uses the internal chat.openai.com API.

WARNING: This is experimental and unofficial. It may:
- Break at any time when OpenAI changes their internal API
- Violate OpenAI's Terms of Service
- Have different rate limits than the API (governed by subscription tier)

Authentication:
    Requires an access token from an authenticated ChatGPT browser session.
    Visit https://chat.openai.com/api/auth/session while logged in to get
    the ``accessToken`` value.
"""

from __future__ import annotations

import json
import uuid
from collections.abc import AsyncIterator
from typing import Any

import httpx

from app.providers.base import (
    ChatResult,
    ChatStreamEvent,
    Message,
    ModelInfo,
    ToolSpec,
)
from app.providers.subscription.base import SubscriptionProvider

# Models typically available on ChatGPT Plus.
CHATGPT_PLUS_MODELS = [
    "gpt-4o",
    "gpt-4o-mini",
    "o3-mini",
]


class ChatGPTPlusProvider(SubscriptionProvider):
    """Experimental adapter for ChatGPT Plus subscriptions.

    Usage::

        provider = ChatGPTPlusProvider(
            session_token="eyJhb...",  # accessToken from session endpoint
        )
        result = await provider.chat_completion(messages, model="gpt-4o")
    """

    name = "subscription/chatgpt_plus"

    def __init__(
        self,
        *,
        session_token: str,
        default_model: str | None = None,
        timeout_s: float = 120.0,
    ) -> None:
        super().__init__(
            session_token=session_token,
            base_url="https://chat.openai.com/backend-api",
            default_model=default_model or CHATGPT_PLUS_MODELS[0],
            timeout_s=timeout_s,
        )

    def _auth_headers(self) -> dict[str, str]:
        """ChatGPT uses Bearer token auth."""
        return {
            "Authorization": f"Bearer {self._session_token}",
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
        """Build ChatGPT internal API payload.

        The internal API uses a different format than the official OpenAI API.
        Messages are wrapped in a specific structure with message IDs.
        """
        # Convert to ChatGPT conversation format.
        chat_messages = []
        for msg in messages:
            if msg.role == "system":
                # System messages handled via system_hints in the internal API.
                continue
            chat_messages.append({
                "id": str(uuid.uuid4()),
                "author": {"role": msg.role},
                "content": {
                    "content_type": "text",
                    "parts": [msg.content or ""],
                },
            })

        payload: dict[str, Any] = {
            "action": "next",
            "messages": chat_messages,
            "model": model,
            "temperature": temperature,
            "stream": stream,
        }

        # Extract system prompt if present.
        system_msgs = [m for m in messages if m.role == "system"]
        if system_msgs:
            system_text = "\n".join(m.content or "" for m in system_msgs)
            payload["system_hints"] = [{"text": system_text}]

        return payload

    def _parse_response(self, data: dict[str, Any]) -> ChatResult:
        """Parse ChatGPT backend-api response.

        Note: The internal API typically only supports streaming.
        This handles the case where a full response is returned.
        """
        content = ""
        message = data.get("message", {})
        msg_content = message.get("content", {})
        parts = msg_content.get("parts", [])
        if parts:
            content = "".join(str(p) for p in parts)

        return ChatResult(
            content=content or None,
            finish_reason="end_turn",
        )

    async def _parse_stream(
        self, response: httpx.Response
    ) -> AsyncIterator[ChatStreamEvent]:
        """Parse SSE stream from ChatGPT backend-api.

        The stream format is:
            data: {"message": {"content": {"parts": ["..."]}}, ...}
            data: [DONE]
        """
        last_content = ""

        async for line in response.aiter_lines():
            if not line.startswith("data: "):
                continue
            data_str = line[6:].strip()
            if data_str == "[DONE]":
                yield ChatStreamEvent(finish=True, finish_reason="end_turn")
                return

            try:
                data = json.loads(data_str)
            except json.JSONDecodeError:
                continue

            # Extract message content.
            message = data.get("message")
            if not message:
                continue

            msg_content = message.get("content", {})
            parts = msg_content.get("parts", [])
            if not parts:
                continue

            full_text = "".join(str(p) for p in parts)

            # Emit only the delta (new content since last event).
            if len(full_text) > len(last_content):
                delta = full_text[len(last_content):]
                last_content = full_text
                yield ChatStreamEvent(delta=delta)

            # Check for completion.
            if message.get("end_turn"):
                yield ChatStreamEvent(finish=True, finish_reason="end_turn")
                return

    async def list_models(self) -> list[ModelInfo]:
        """Return models available on ChatGPT Plus."""
        return [ModelInfo(id=m) for m in CHATGPT_PLUS_MODELS]
