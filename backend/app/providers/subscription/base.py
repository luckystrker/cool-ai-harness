"""Base class for subscription provider adapters.

Subscription adapters authenticate via session tokens/cookies rather than
API keys. They share common patterns for session management and rate limiting.
"""

from __future__ import annotations

from abc import abstractmethod
from collections.abc import AsyncIterator
from typing import Any

import httpx

from app.providers.base import (
    ChatResult,
    ChatStreamEvent,
    LLMProvider,
    Message,
    ModelInfo,
    ToolSpec,
)


class SubscriptionProvider(LLMProvider):
    """Base class for subscription-based LLM providers.

    Subclasses implement the actual HTTP calls to the service's internal API.
    Authentication is via session tokens extracted from the user's browser.

    Experimental: these adapters may break without notice as they rely on
    unofficial endpoints.
    """

    name: str = "subscription"

    def __init__(
        self,
        *,
        session_token: str,
        base_url: str,
        default_model: str | None = None,
        timeout_s: float = 120.0,
    ) -> None:
        self._session_token = session_token
        self._base_url = base_url.rstrip("/")
        self._default_model = default_model
        self._timeout = timeout_s
        self._client: httpx.AsyncClient | None = None

    async def _get_client(self) -> httpx.AsyncClient:
        """Lazy-init the HTTP client with auth headers."""
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                base_url=self._base_url,
                timeout=httpx.Timeout(self._timeout, connect=10.0),
                headers=self._auth_headers(),
            )
        return self._client

    @abstractmethod
    def _auth_headers(self) -> dict[str, str]:
        """Return authentication headers for the service."""
        ...

    @abstractmethod
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
        """Build the service-specific request payload."""
        ...

    @abstractmethod
    def _parse_response(self, data: dict[str, Any]) -> ChatResult:
        """Parse a non-streaming response into ChatResult."""
        ...

    @abstractmethod
    async def _parse_stream(
        self, response: httpx.Response
    ) -> AsyncIterator[ChatStreamEvent]:
        """Parse a streaming response into ChatStreamEvent objects."""
        ...
        yield  # pragma: no cover

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
        """Non-streaming completion via the subscription service."""
        client = await self._get_client()
        payload = self._build_request_payload(
            messages,
            model=model,
            tools=tools,
            temperature=temperature,
            max_tokens=max_tokens,
            stream=False,
            **kwargs,
        )
        response = await client.post("/chat", json=payload)
        response.raise_for_status()
        return self._parse_response(response.json())

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
        """Streaming completion via the subscription service."""
        client = await self._get_client()
        payload = self._build_request_payload(
            messages,
            model=model,
            tools=tools,
            temperature=temperature,
            max_tokens=max_tokens,
            stream=True,
            **kwargs,
        )
        async with client.stream("POST", "/chat", json=payload) as response:
            response.raise_for_status()
            async for event in self._parse_stream(response):
                yield event

    async def list_models(self) -> list[ModelInfo]:
        """Subscription services typically expose a fixed model set."""
        if self._default_model:
            return [ModelInfo(id=self._default_model)]
        return []

    async def close(self) -> None:
        """Close the HTTP client."""
        if self._client and not self._client.is_closed:
            await self._client.aclose()
