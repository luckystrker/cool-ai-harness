"""Provider abstraction.

The harness talks to every LLM through a single `LLMProvider` interface, so
swapping OpenAI / Anthropic / local / subscription-backed providers never
requires changes in the agent loop or tools layer.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any, Literal

# --- Unified message / tool schema (internal canonical format) ---

MessageRole = Literal["system", "user", "assistant", "tool"]


@dataclass
class Message:
    """A single chat message in the harness's canonical format."""

    role: MessageRole
    content: str | None = None
    # Assistant tool calls (when role == "assistant").
    tool_calls: list[dict[str, Any]] | None = None
    # Tool result reference (when role == "tool").
    tool_call_id: str | None = None
    name: str | None = None  # tool name, for role == "tool"


@dataclass
class ToolSpec:
    """Definition of a tool the model is allowed to call (OpenAI-style schema)."""

    name: str
    description: str
    parameters: dict[str, Any] = field(default_factory=lambda: {"type": "object", "properties": {}})


@dataclass
class Usage:
    """Token accounting for a single completion."""

    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0
    cost_usd: float | None = None


@dataclass
class ChatResult:
    """Non-streaming completion result."""

    content: str | None
    tool_calls: list[dict[str, Any]] | None = None
    usage: Usage | None = None
    finish_reason: str | None = None
    raw: Any | None = None  # provider-native payload, for debugging


@dataclass
class ChatStreamEvent:
    """One event in a streaming completion.

    - delta: incremental text (may be empty)
    - tool_call_delta: incremental tool call fragment (provider-shaped)
    - finish: True on the terminal event (carries usage / finish_reason)
    """

    delta: str = ""
    tool_call_delta: dict[str, Any] | None = None
    finish: bool = False
    finish_reason: str | None = None
    usage: Usage | None = None


class LLMProvider(ABC):
    """Abstract base every provider implements.

    Concrete providers (openai, anthropic, subscription/..., local ollama) are
    constructed with resolved credentials (already decrypted) by the registry.
    """

    name: str = "base"

    @abstractmethod
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
        """Return a full (non-streaming) completion."""

    @abstractmethod
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
        """Yield incremental ChatStreamEvent objects until a finish event."""

    async def embed(self, texts: list[str], *, model: str | None = None) -> list[list[float]]:
        """Optional. Providers that don't support embeddings should override."""
        raise NotImplementedError(f"{self.name} does not implement embed()")
