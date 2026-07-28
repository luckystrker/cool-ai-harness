"""OpenTelemetry span export (Фаза 3a §5 — optional OTel integration).

When ``settings.otel_exporter_endpoint`` is set, this module exports LLM
calls and tool invocations as OTel spans to the configured OTLP HTTP
endpoint. When unset (default), all functions are no-ops.

Usage from the agent loop / runners:
    from app.observability.otel import emit_llm_span, emit_tool_span

The implementation is intentionally lazy: the OTel SDK is imported only when
the feature is enabled, so the base install doesn't require opentelemetry
packages. Install them with:
    pip install opentelemetry-api opentelemetry-sdk opentelemetry-exporter-otlp-proto-http
"""

from __future__ import annotations

from typing import Any

from app.core.logging import get_logger

log = get_logger(__name__)

# Lazy-initialized tracer (None = disabled or not yet initialized).
_tracer: Any = None
_initialized: bool = False


def _ensure_initialized() -> bool:
    """Lazily initialize the OTel tracer on first use. Returns True if ready."""
    global _tracer, _initialized
    if _initialized:
        return _tracer is not None
    _initialized = True

    from app.core.config import get_settings

    settings = get_settings()
    endpoint = settings.otel_exporter_endpoint
    if not endpoint:
        return False

    try:
        from opentelemetry import trace
        from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
        from opentelemetry.sdk.resources import Resource
        from opentelemetry.sdk.trace import TracerProvider
        from opentelemetry.sdk.trace.export import BatchSpanProcessor

        resource = Resource.create({"service.name": settings.otel_service_name})
        provider = TracerProvider(resource=resource)
        exporter = OTLPSpanExporter(endpoint=f"{endpoint.rstrip('/')}/v1/traces")
        provider.add_span_processor(BatchSpanProcessor(exporter))
        trace.set_tracer_provider(provider)
        _tracer = trace.get_tracer("cool-ai-harness")
        log.info("otel.initialized", endpoint=endpoint)
        return True
    except ImportError:
        log.warning(
            "otel.sdk_not_installed",
            hint="pip install opentelemetry-api opentelemetry-sdk opentelemetry-exporter-otlp-proto-http",
        )
        return False
    except Exception as exc:
        log.warning("otel.init_failed", error=str(exc))
        return False


def emit_llm_span(
    *,
    model: str,
    provider: str,
    prompt_tokens: int = 0,
    completion_tokens: int = 0,
    total_tokens: int = 0,
    cost_usd: float = 0.0,
    duration_ms: int = 0,
    run_id: int | None = None,
    conversation_id: int | None = None,
) -> None:
    """Export an LLM call as an OTel span (no-op when OTel is disabled)."""
    if not _ensure_initialized():
        return
    try:
        from opentelemetry import trace

        with _tracer.start_as_current_span(
            f"llm.call {model}",
            kind=trace.SpanKind.CLIENT,
        ) as span:
            span.set_attribute("llm.model", model)
            span.set_attribute("llm.provider", provider)
            span.set_attribute("llm.usage.prompt_tokens", prompt_tokens)
            span.set_attribute("llm.usage.completion_tokens", completion_tokens)
            span.set_attribute("llm.usage.total_tokens", total_tokens)
            span.set_attribute("llm.cost_usd", cost_usd)
            span.set_attribute("llm.duration_ms", duration_ms)
            if run_id is not None:
                span.set_attribute("run.id", run_id)
            if conversation_id is not None:
                span.set_attribute("conversation.id", conversation_id)
    except Exception as exc:
        log.debug("otel.emit_llm_span_failed", error=str(exc))


def emit_tool_span(
    *,
    name: str,
    duration_ms: int = 0,
    success: bool = True,
    error: str | None = None,
    run_id: int | None = None,
    conversation_id: int | None = None,
) -> None:
    """Export a tool invocation as an OTel span (no-op when OTel is disabled)."""
    if not _ensure_initialized():
        return
    try:
        from opentelemetry import trace

        with _tracer.start_as_current_span(
            f"tool.{name}",
            kind=trace.SpanKind.INTERNAL,
        ) as span:
            span.set_attribute("tool.name", name)
            span.set_attribute("tool.duration_ms", duration_ms)
            span.set_attribute("tool.success", success)
            if error:
                span.set_attribute("tool.error", error)
                span.set_status(trace.StatusCode.ERROR, error)
            if run_id is not None:
                span.set_attribute("run.id", run_id)
            if conversation_id is not None:
                span.set_attribute("conversation.id", conversation_id)
    except Exception as exc:
        log.debug("otel.emit_tool_span_failed", error=str(exc))
