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
    # Provider reasoning / thinking trace, when the model exposes one
    # (DeepSeek `reasoning_content`, OpenRouter `reasoning`, etc.). None when
    # the model produced no reasoning block.
    reasoning: str | None = None
    raw: Any | None = None  # provider-native payload, for debugging


@dataclass
class ChatStreamEvent:
    """One event in a streaming completion.

    - delta: incremental text (may be empty)
    - reasoning: incremental reasoning / thinking text (may be empty). Providers
      that surface a chain-of-thought (DeepSeek `reasoning_content`, OpenRouter
      `reasoning`, etc.) emit it here; others leave it empty.
    - tool_call_delta: incremental tool call fragment (provider-shaped)
    - finish: True on the terminal event (carries usage / finish_reason)
    """

    delta: str = ""
    reasoning: str = ""
    # OpenAI streams delta.tool_calls as a LIST of partial tool-call objects,
    # so providers should emit that list here (even for a single call). The
    # agent loop's _merge_tool_call_deltas() also tolerates a bare dict for
    # convenience/test doubles.
    tool_call_delta: list[dict[str, Any]] | dict[str, Any] | None = None
    finish: bool = False
    finish_reason: str | None = None
    usage: Usage | None = None


@dataclass
class ModelInfo:
    """One model offered by a provider, with whatever metadata is available.

    Fields are optional because providers expose them inconsistently: OpenAI's
    ``/models`` returns only an id, while OpenRouter / Groq / Ollama also
    include a context length. Prices are filled in from the local pricing table
    (``app.providers.pricing``) when the id matches a known entry.
    """

    id: str
    context_window: int | None = None
    prompt_price: float | None = None
    completion_price: float | None = None


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

    async def list_models(self) -> list[ModelInfo]:
        """List models the provider serves, with optional metadata.

        Optional. Used by the provider settings UI / model picker. Providers
        that expose an OpenAI-compatible ``GET /models`` endpoint (most
        OpenAI-compatible backends + Anthropic) should override this to return
        real data; the base implementation signals "unsupported".
        """
        raise NotImplementedError(f"{self.name} does not implement list_models()")
