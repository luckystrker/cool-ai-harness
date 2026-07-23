"""Secret masking for tool outputs, messages, traces, and logs.

Scans text for common secret patterns (API keys, bearer tokens, passwords,
private keys) and replaces them with ``[REDACTED:<type>]``. Applied to:

- Tool results before they're shown to the model and the UI
- Log messages (via a structlog processor, Фаза 3a)
- Agent event payloads that might contain echoed secrets

The patterns are intentionally conservative: we'd rather miss a secret than
false-positive on normal text. Each pattern targets a specific, high-signal
format.
"""

from __future__ import annotations

import re
from typing import Any

from app.core.logging import get_logger

log = get_logger(__name__)

# --- Patterns ---------------------------------------------------------------
# Each pattern matches a specific secret format. The replacement preserves the
# prefix (so logs remain debuggable) but redacts the secret value.

# Bearer tokens: "Bearer eyJ..." or "bearer abc123"
_RE_BEARER = re.compile(
    r"(?i)(bearer\s+)([A-Za-z0-9\-._~+\/=]{20,})",
)

# API keys with common prefixes: sk-..., key-..., pk_..., etc.
_RE_API_KEY = re.compile(
    r"\b((?:sk|pk|rk|key|api[_-]?key|token)[_-])([A-Za-z0-9\-_]{20,})",
    re.IGNORECASE,
)

# Generic key=value assignments in env-like contexts:
# API_KEY=..., SECRET=..., PASSWORD=..., TOKEN=...
# Negative lookahead prevents re-matching already-redacted [REDACTED:...] text.
_RE_ENV_ASSIGN = re.compile(
    r"(?i)((?:api[_-]?key|secret|password|passwd|token|auth|credential|private[_-]?key)"
    r"\s*[:=]\s*)(?!\[REDACTED)([^\s,;'\"\n\r]{8,})",
)

# AWS access keys: AKIA...
_RE_AWS = re.compile(r"\b(AKIA[A-Z0-9]{16})\b")

# AWS secret keys (40-char base64 after known prefix)
_RE_AWS_SECRET = re.compile(r"\b([A-Za-z0-9/+=]{40})\b")

# Private key blocks (PEM)
_RE_PEM = re.compile(
    r"(-----BEGIN (?:RSA |EC |OPENSSH |DSA |)PRIVATE KEY-----)"
    r"[\s\S]*?"
    r"(-----END (?:RSA |EC |OPENSSH |DSA |)PRIVATE KEY-----)",
)

# GitHub tokens: ghp_..., gho_..., ghs_..., ghu_...
_RE_GITHUB = re.compile(r"\b(gh[pousr]_[A-Za-z0-9]{36})\b")

# Slack tokens: xox[bpsa]-...
_RE_SLACK = re.compile(r"\b(xox[bpsa]-[A-Za-z0-9\-]{10,})\b")

# Generic long hex/base64 strings that look like secrets (48+ chars, no spaces)
# Only matched when preceded by a secret-suggestive keyword.
_RE_LONG_SECRET = re.compile(
    r"(?i)(secret|token|key|password|credential)\s*[:=]\s*([A-Fa-f0-9]{48,})",
)

# Patterns to apply in order. Each is (pattern, replacement_template).
# The replacement uses \1 for the prefix and a redaction marker for the value.
_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (_RE_PEM, "[REDACTED:private-key]"),
    (_RE_BEARER, r"\1[REDACTED:bearer]"),
    (_RE_GITHUB, "[REDACTED:github-token]"),
    (_RE_SLACK, "[REDACTED:slack-token]"),
    (_RE_AWS, "[REDACTED:aws-key]"),
    (_RE_API_KEY, r"\1[REDACTED:api-key]"),
    (_RE_ENV_ASSIGN, r"\1[REDACTED]"),
    (_RE_LONG_SECRET, r"\1[REDACTED:hex]"),
]


def mask_secrets(text: str, *, enabled: bool = True) -> str:
    """Mask secrets in a string.

    When ``enabled`` is False, returns the text unchanged (used to bypass
    masking in trusted contexts like internal tests).

    The function is idempotent: already-redacted text won't be re-processed
    (``[REDACTED...]`` doesn't match any pattern).
    """
    if not enabled or not text:
        return text
    result = text
    for pattern, replacement in _PATTERNS:
        result = pattern.sub(replacement, result)
    return result


def mask_secrets_in_value(value: Any, *, enabled: bool = True) -> Any:
    """Recursively mask secrets in a value (str, dict, list).

    Walks dicts and lists, masking every string leaf. Non-string values pass
    through unchanged.
    """
    if not enabled:
        return value
    if isinstance(value, str):
        return mask_secrets(value, enabled=True)
    if isinstance(value, dict):
        return {k: mask_secrets_in_value(v, enabled=True) for k, v in value.items()}
    if isinstance(value, list):
        return [mask_secrets_in_value(v, enabled=True) for v in value]
    return value


def mask_tool_output(output: str) -> str:
    """Convenience: mask secrets in a tool's output string."""
    from app.core.config import get_settings

    return mask_secrets(output, enabled=get_settings().mask_secrets)
