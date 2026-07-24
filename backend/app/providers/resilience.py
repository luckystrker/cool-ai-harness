"""Provider resilience: retry with backoff, circuit breaker, fallback (Фаза 1.5 §5).

``ResilientProvider`` is a decorator over the ``LLMProvider`` ABC. It wraps a
primary provider plus an ordered list of fallback providers and transparently:

  - retries retriable failures (HTTP 429 / 5xx, timeouts, network errors) with
    exponential backoff + full jitter;
  - trips a per-provider circuit breaker after a run of failures and short-
    circuits straight to the next fallback;
  - falls back to the next provider in the chain when the current one exhausts
    its retries.

Because it implements the same ``LLMProvider`` interface, the agent loop and
executor call it unchanged — they don't know retry/fallback is happening.

Streaming caveat: retry/fallback is only safe *before* the stream has emitted
any event. Once bytes have flowed to the caller, re-running the request would
duplicate output, so a failure mid-stream is not retried — it propagates (and,
if it leaves the provider's circuit on the failure path, the breaker still
records it for the *next* turn).

The circuit-breaker state lives in a process-wide singleton
(:func:`get_circuit_registry`) — same trade-off as ``app/agent/runs.py``:
correct within one process, not shared across workers (would need Redis for
multi-process deployments; out of scope here).
"""

from __future__ import annotations

import asyncio
import random
import time
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any

import httpx

from app.core.config import get_settings
from app.core.logging import get_logger
from app.providers.base import (
    ChatResult,
    ChatStreamEvent,
    LLMProvider,
    Message,
    ToolSpec,
)

log = get_logger(__name__)


# --- Retriable-error classification -----------------------------------------


def _is_retriable(exc: BaseException) -> bool:
    """Whether ``exc`` is a transient failure worth retrying/falling back."""
    if isinstance(exc, httpx.TimeoutException):
        return True
    if isinstance(exc, httpx.NetworkError):
        return True
    if isinstance(exc, httpx.HTTPStatusError):
        status = exc.response.status_code if exc.response is not None else 0
        # 429 (rate limit) and 5xx (server errors) are transient.
        return status == 429 or 500 <= status < 600
    return False


# --- Circuit breaker --------------------------------------------------------


@dataclass
class _CircuitState:
    """Mutable circuit-breaker state for one provider name."""

    consecutive_failures: int = 0
    opened_at: float | None = None  # monotonic timestamp when the breaker opened
    # When True, the breaker is half-open: the next call is a single probe; on
    # success the breaker closes, on failure it re-opens for another reset window.
    half_open: bool = False


@dataclass
class CircuitRegistry:
    """Process-wide registry of per-provider circuit-breaker state.

    A breaker is OPEN (calls short-circuit) once ``consecutive_failures`` reaches
    ``failure_threshold``. It moves to HALF-OPEN after ``reset_s`` elapses; the
    next call is allowed through as a probe. A probe success closes the breaker,
    a probe failure re-opens it.
    """

    failure_threshold: int = 5
    reset_s: float = 60.0
    _states: dict[str, _CircuitState] = field(default_factory=dict)

    def _state(self, name: str) -> _CircuitState:
        return self._states.setdefault(name, _CircuitState())

    def is_open(self, name: str) -> bool:
        """Whether calls to ``name`` should be short-circuited right now.

        Handles the open→half-open transition based on the reset window.
        Returns False (allow the call) when half-open so a probe can run.
        """
        st = self._state(name)
        if st.opened_at is None:
            return False
        if time.monotonic() - st.opened_at >= self.reset_s:
            # Window elapsed: move to half-open and allow a single probe.
            if not st.half_open:
                st.half_open = True
                log.info("providers.circuit.half_open", provider=name)
            return False
        return True

    def record_success(self, name: str) -> None:
        st = self._state(name)
        was_open = st.opened_at is not None
        st.consecutive_failures = 0
        st.opened_at = None
        st.half_open = False
        if was_open:
            log.info("providers.circuit.closed", provider=name)

    def record_failure(self, name: str) -> None:
        st = self._state(name)
        st.consecutive_failures += 1
        if st.opened_at is None and st.consecutive_failures >= self.failure_threshold:
            st.opened_at = time.monotonic()
            st.half_open = False
            log.warning(
                "providers.circuit.opened",
                provider=name,
                failures=st.consecutive_failures,
                threshold=self.failure_threshold,
            )
        elif st.half_open:
            # A probe failed: re-open for a fresh reset window.
            st.opened_at = time.monotonic()
            st.half_open = False
            log.warning("providers.circuit.reopened", provider=name)


_REGISTRY: CircuitRegistry | None = None


def get_circuit_registry() -> CircuitRegistry:
    """Process-wide circuit registry, lazily configured from settings."""
    global _REGISTRY
    if _REGISTRY is None:
        s = get_settings()
        _REGISTRY = CircuitRegistry(
            failure_threshold=s.provider_circuit_failure_threshold,
            reset_s=s.provider_circuit_reset_s,
        )
    return _REGISTRY


def reset_circuit_registry() -> None:
    """Drop all breaker state (used by tests)."""
    global _REGISTRY
    if _REGISTRY is not None:
        _REGISTRY._states.clear()


# --- Backoff ----------------------------------------------------------------


async def _backoff_delay(attempt: int, *, base: float, cap: float) -> float:
    """Exponential backoff with full jitter: ``random.uniform(0, min(cap, base*2**attempt))``."""
    expo = min(cap, base * (2**attempt))
    delay = random.uniform(0, expo)
    await asyncio.sleep(delay)
    return delay


# --- Resilient provider -----------------------------------------------------


class ResilientProvider(LLMProvider):
    """Wraps a primary provider with retry/backoff/circuit-breaker/fallback.

    Args:
        primary: The preferred provider.
        fallbacks: Ordered providers tried after the primary is exhausted.
        max_retries: Per-provider retry attempts on retriable failures.
        retry_base_delay_s / retry_max_delay_s: Backoff tuning.
        name: Display name (defaults to the primary's name).
    """

    def __init__(
        self,
        primary: LLMProvider,
        fallbacks: list[LLMProvider] | None = None,
        *,
        max_retries: int | None = None,
        retry_base_delay_s: float | None = None,
        retry_max_delay_s: float | None = None,
        name: str | None = None,
    ) -> None:
        self.primary = primary
        self.fallbacks = list(fallbacks or [])
        s = get_settings()
        self.max_retries = max_retries if max_retries is not None else s.provider_max_retries
        self.retry_base_delay_s = (
            retry_base_delay_s if retry_base_delay_s is not None else s.provider_retry_base_delay_s
        )
        self.retry_max_delay_s = (
            retry_max_delay_s if retry_max_delay_s is not None else s.provider_retry_max_delay_s
        )
        self.name = name or primary.name
        self._circuit = get_circuit_registry()

    @property
    def chain(self) -> list[LLMProvider]:
        return [self.primary, *self.fallbacks]

    # The non-streaming path mirrors the streaming one but is simpler (no
    # partial-output concern). It's not used by the agent loop today (which
    # streams), but completing the ABC contract keeps the decorator honest.
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
        last_exc: BaseException | None = None
        for provider in self.chain:
            if self._circuit.is_open(provider.name):
                log.info("providers.circuit.skipping", provider=provider.name)
                continue
            for attempt in range(self.max_retries + 1):
                try:
                    result = await provider.chat_completion(
                        messages,
                        model=model,
                        tools=tools,
                        temperature=temperature,
                        max_tokens=max_tokens,
                        **kwargs,
                    )
                    self._circuit.record_success(provider.name)
                    return result
                except Exception as exc:  # noqa: BLE001 — classify below
                    if not _is_retriable(exc):
                        self._circuit.record_success(provider.name)
                        raise
                    last_exc = exc
                    if attempt < self.max_retries:
                        delay = await _backoff_delay(
                            attempt,
                            base=self.retry_base_delay_s,
                            cap=self.retry_max_delay_s,
                        )
                        log.warning(
                            "providers.retry",
                            provider=provider.name,
                            attempt=attempt + 1,
                            delay_s=round(delay, 3),
                            error=str(exc),
                        )
                        continue
                    self._circuit.record_failure(provider.name)
                    log.warning(
                        "providers.retries_exhausted",
                        provider=provider.name,
                        attempts=self.max_retries + 1,
                    )
                    break  # try next fallback
        assert last_exc is not None  # chain was non-empty and all failed
        raise last_exc

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
        last_exc: BaseException | None = None
        for provider in self.chain:
            if self._circuit.is_open(provider.name):
                log.info("providers.circuit.skipping", provider=provider.name)
                continue
            for attempt in range(self.max_retries + 1):
                emitted = False
                try:
                    async for event in provider.chat_completion_stream(
                        messages,
                        model=model,
                        tools=tools,
                        temperature=temperature,
                        max_tokens=max_tokens,
                        **kwargs,
                    ):
                        emitted = True
                        yield event
                    self._circuit.record_success(provider.name)
                    return
                except Exception as exc:  # noqa: BLE001 — classify below
                    if not _is_retriable(exc):
                        # Non-retriable: record success (no provider outage) and
                        # propagate — e.g. a 400 bad request is a caller bug.
                        self._circuit.record_success(provider.name)
                        raise
                    last_exc = exc
                    # Once the stream has emitted output we can't safely retry
                    # (re-running would duplicate tokens). Treat as terminal for
                    # this provider: record the failure and propagate.
                    if emitted:
                        self._circuit.record_failure(provider.name)
                        raise
                    if attempt < self.max_retries:
                        delay = await _backoff_delay(
                            attempt,
                            base=self.retry_base_delay_s,
                            cap=self.retry_max_delay_s,
                        )
                        log.warning(
                            "providers.retry.stream",
                            provider=provider.name,
                            attempt=attempt + 1,
                            delay_s=round(delay, 3),
                            error=str(exc),
                        )
                        continue
                    self._circuit.record_failure(provider.name)
                    log.warning(
                        "providers.retries_exhausted.stream",
                        provider=provider.name,
                        attempts=self.max_retries + 1,
                    )
                    break  # try next fallback
        assert last_exc is not None
        raise last_exc

    async def embed(self, texts: list[str], *, model: str | None = None) -> list[list[float]]:
        # Embeddings are not part of the resilience contract — delegate to primary.
        return await self.primary.embed(texts, model=model)
