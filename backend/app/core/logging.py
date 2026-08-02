"""Structured logging setup using structlog."""

from __future__ import annotations

import logging
import re
import sys

import structlog

from app.core.config import get_settings

# Patterns that look like secrets in log values (API keys, tokens, passwords).
_SECRET_PATTERNS = [
    re.compile(r"(?i)(api[_-]?key|secret|token|password|passwd|credential)\s*[=:]\s*\S+"),
    re.compile(r"\bsk-[A-Za-z0-9]{20,}\b"),  # OpenAI-style keys
    re.compile(r"\bsk-ant-[A-Za-z0-9-]{20,}\b"),  # Anthropic-style keys
    re.compile(r"(?i)bearer\s+[A-Za-z0-9._-]{20,}"),
]


def _mask_secrets_processor(
    logger: object, method_name: str, event_dict: dict
) -> dict:
    """Structlog processor: mask secret-looking values in log event strings."""
    for key, value in list(event_dict.items()):
        if isinstance(value, str) and len(value) > 10:
            for pattern in _SECRET_PATTERNS:
                if pattern.search(value):
                    event_dict[key] = pattern.sub("[REDACTED]", value)
                    break
    return event_dict


def configure_logging() -> None:
    """Configure structlog + stdlib logging once at startup."""
    settings = get_settings()
    level = logging.DEBUG if settings.debug else logging.INFO

    # Shared timestamper for stdlib logs routed through structlog.
    shared_processors: list = [
        structlog.contextvars.merge_contextvars,
        structlog.processors.add_log_level,
        structlog.processors.TimeStamper(fmt="iso"),
        _mask_secrets_processor,
    ]

    structlog.configure(
        processors=[
            *shared_processors,
            structlog.stdlib.ProcessorFormatter.wrap_for_formatter,
        ],
        # Use stdlib-based logger factory so stdlib logging calls (e.g. with
        # extra=...) and structlog loggers share a single rendering pipeline.
        logger_factory=structlog.stdlib.LoggerFactory(),
        wrapper_class=structlog.stdlib.BoundLogger,
        cache_logger_on_first_use=True,
    )

    # Stdlib logging → structlog formatter.
    formatter = structlog.stdlib.ProcessorFormatter(
        foreign_pre_chain=shared_processors,
        processors=[
            structlog.stdlib.ProcessorFormatter.remove_processors_meta,
            structlog.dev.ConsoleRenderer(),
        ],
    )

    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(formatter)

    root = logging.getLogger()
    root.handlers.clear()
    root.addHandler(handler)
    root.setLevel(level)

    # Quiet down noisy libs.
    for noisy in ("httpx", "httpcore", "watchfiles", "sqlalchemy.engine"):
        logging.getLogger(noisy).setLevel(logging.WARNING)


def get_logger(name: str | None = None) -> structlog.stdlib.BoundLogger:
    """Return a bound structlog logger."""
    return structlog.get_logger(name)
