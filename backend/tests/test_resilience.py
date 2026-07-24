"""Tests for ResilientProvider: retry/backoff, circuit breaker, fallback (Фаза 1.5 §5)."""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Any

import httpx
import pytest

from app.providers import ChatStreamEvent, LLMProvider, Message, ToolSpec, Usage
from app.providers.resilience import (
    CircuitRegistry,
    ResilientProvider,
    _is_retriable,
    get_circuit_registry,
    reset_circuit_registry,
)


# --- helpers -----------------------------------------------------------------


class _StubProvider(LLMProvider):
    """A streaming provider whose behavior is controlled per-call.

    ``failures`` is a list of exceptions to raise on successive calls; once
    exhausted, the provider streams ``text`` and finishes. ``name`` lets each
    stub impersonate a distinct provider for circuit-breaker bookkeeping.
    """

    def __init__(
        self,
        name: str,
        *,
        text: str = "ok",
        failures: list[BaseException] | None = None,
        usage_cost: float | None = 0.001,
    ) -> None:
        self.name = name
        self._text = text
        self._failures = list(failures or [])
        self._usage_cost = usage_cost
        self.call_count = 0

    async def chat_completion(self, messages, *, model, tools=None, **kwargs):  # type: ignore[override]
        raise NotImplementedError

    async def chat_completion_stream(  # type: ignore[override]
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        **kwargs: Any,
    ) -> AsyncIterator[ChatStreamEvent]:
        self.call_count += 1
        if self._failures:
            raise self._failures.pop(0)
        yield ChatStreamEvent(delta=self._text)
        yield ChatStreamEvent(
            finish=True,
            usage=Usage(prompt_tokens=10, completion_tokens=5, total_tokens=15, cost_usd=self._usage_cost),
        )


def _http_status_error(status: int) -> httpx.HTTPStatusError:
    request = httpx.Request("POST", "https://example.com/chat")
    response = httpx.Response(status, request=request)
    return httpx.HTTPStatusError("error", request=request, response=response)


@pytest.fixture(autouse=True)
def _reset_circuit():
    """Each test starts with a clean circuit registry."""
    reset_circuit_registry()
    get_circuit_registry()  # ensure configured
    yield
    reset_circuit_registry()


# --- classification ----------------------------------------------------------


def test_retriable_classification() -> None:
    assert _is_retriable(_http_status_error(429)) is True
    assert _is_retriable(_http_status_error(500)) is True
    assert _is_retriable(_http_status_error(503)) is True
    assert _is_retriable(httpx.ConnectTimeout("timeout")) is True
    assert _is_retriable(httpx.ReadError("network")) is True
    # Non-retriable.
    assert _is_retriable(_http_status_error(400)) is False
    assert _is_retriable(_http_status_error(401)) is False
    assert _is_retriable(ValueError("not http")) is False


# --- retry -------------------------------------------------------------------


@pytest.mark.asyncio
async def test_retry_then_success() -> None:
    """A transient 429 is retried and the stream eventually succeeds."""
    primary = _StubProvider(
        "p", failures=[_http_status_error(429), _http_status_error(429)]
    )
    rp = ResilientProvider(
        primary, max_retries=3, retry_base_delay_s=0.0, retry_max_delay_s=0.0
    )
    events = [e async for e in rp.chat_completion_stream([], model="m")]
    assert any(e.delta for e in events)
    assert events[-1].finish
    # Initial attempt + 2 retries = 3 calls.
    assert primary.call_count == 3


@pytest.mark.asyncio
async def test_non_retriable_propagates_immediately() -> None:
    """A 400 is not retried — it surfaces at once."""
    primary = _StubProvider("p", failures=[_http_status_error(400)])
    rp = ResilientProvider(
        primary, max_retries=3, retry_base_delay_s=0.0, retry_max_delay_s=0.0
    )
    with pytest.raises(httpx.HTTPStatusError):
        [e async for e in rp.chat_completion_stream([], model="m")]
    assert primary.call_count == 1


# --- fallback ----------------------------------------------------------------


@pytest.mark.asyncio
async def test_fallback_when_primary_exhausts_retries() -> None:
    """Primary keeps failing past its retries → fallback serves the request."""
    primary = _StubProvider(
        "p",
        failures=[
            _http_status_error(503),
            _http_status_error(503),
            _http_status_error(503),
            _http_status_error(503),  # 1 initial + 3 retries
        ],
    )
    fallback = _StubProvider("f", text="from fallback")
    rp = ResilientProvider(
        primary,
        fallbacks=[fallback],
        max_retries=3,
        retry_base_delay_s=0.0,
        retry_max_delay_s=0.0,
    )
    events = [e async for e in rp.chat_completion_stream([], model="m")]
    deltas = "".join(e.delta for e in events)
    assert "from fallback" in deltas
    assert primary.call_count == 4  # exhausted
    assert fallback.call_count == 1


@pytest.mark.asyncio
async def test_all_providers_fail_raises_last() -> None:
    primary = _StubProvider("p", failures=[_http_status_error(429), _http_status_error(429)])
    fallback = _StubProvider("f", failures=[_http_status_error(429), _http_status_error(429)])
    rp = ResilientProvider(
        primary,
        fallbacks=[fallback],
        max_retries=1,
        retry_base_delay_s=0.0,
        retry_max_delay_s=0.0,
    )
    with pytest.raises(httpx.HTTPStatusError):
        [e async for e in rp.chat_completion_stream([], model="m")]
    assert primary.call_count == 2  # 1 initial + 1 retry
    assert fallback.call_count == 2


# --- circuit breaker ---------------------------------------------------------


def test_circuit_opens_after_threshold() -> None:
    reg = CircuitRegistry(failure_threshold=3, reset_s=60.0)
    for _ in range(3):
        reg.record_failure("p")
    assert reg.is_open("p") is True


def test_circuit_closed_on_success() -> None:
    reg = CircuitRegistry(failure_threshold=3, reset_s=60.0)
    for _ in range(3):
        reg.record_failure("p")
    assert reg.is_open("p")
    reg.record_success("p")
    assert reg.is_open("p") is False


def test_circuit_half_open_after_reset(monkeypatch) -> None:
    """After the reset window elapses, is_open returns False (probe allowed)."""
    import time as _time

    reg = CircuitRegistry(failure_threshold=1, reset_s=60.0)
    reg.record_failure("p")  # opens immediately
    assert reg.is_open("p")

    t0 = [_time.monotonic()]
    monkeypatch.setattr(_time, "monotonic", lambda: t0[0])

    # Advance time past the reset window.
    t0[0] += 61.0
    assert reg.is_open("p") is False  # half-open: allow a probe
    # A probe failure re-opens it.
    reg.record_failure("p")
    assert reg.is_open("p") is True


@pytest.mark.asyncio
async def test_circuit_skips_open_provider_to_fallback() -> None:
    """Once the primary's circuit is open, calls skip straight to the fallback."""
    # Pre-open the primary's circuit by recording threshold failures.
    reg = get_circuit_registry()
    for _ in range(reg.failure_threshold):
        reg.record_failure("primary")
    assert reg.is_open("primary")

    primary = _StubProvider("primary")  # would succeed if called
    fallback = _StubProvider("fallback", text="via fallback")
    rp = ResilientProvider(
        primary,
        fallbacks=[fallback],
        max_retries=1,
        retry_base_delay_s=0.0,
        retry_max_delay_s=0.0,
    )
    events = [e async for e in rp.chat_completion_stream([], model="m")]
    assert "via fallback" in "".join(e.delta for e in events)
    assert primary.call_count == 0  # skipped because circuit open
    assert fallback.call_count == 1


@pytest.mark.asyncio
async def test_no_retry_after_partial_output() -> None:
    """A failure mid-stream (after tokens emitted) is NOT retried — it propagates.

    Re-running the request would duplicate already-delivered output, so once the
    stream has produced anything, a failure is terminal for that provider.
    """

    class _PartialFail(LLMProvider):
        name = "p"

        def __init__(self):
            self.call_count = 0

        async def chat_completion(self, messages, *, model, tools=None, **kwargs):  # type: ignore[override]
            raise NotImplementedError

        async def chat_completion_stream(self, messages, *, model, **kwargs):  # type: ignore[override]
            self.call_count += 1
            yield ChatStreamEvent(delta="partial-")
            raise _http_status_error(500)  # mid-stream failure

    rp = ResilientProvider(
        _PartialFail(),
        max_retries=3,
        retry_base_delay_s=0.0,
        retry_max_delay_s=0.0,
    )
    with pytest.raises(httpx.HTTPStatusError):
        [e async for e in rp.chat_completion_stream([], model="m")]
    # No retry happened — the failure was after partial output.
