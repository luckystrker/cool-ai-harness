"""Sandbox environment preparation for code execution.

Strips secret-looking environment variables before spawning subprocesses so
that untrusted code can't exfiltrate host secrets via ``os.environ``.

This is NOT a full sandbox (no filesystem isolation, no CPU/memory caps beyond
what the subprocess tool already does). Full Docker-container sandboxing lands
in Фаза 4. For now, env-var stripping closes the most obvious exfiltration
vector.
"""

from __future__ import annotations

import os
import re

from app.core.logging import get_logger

log = get_logger(__name__)

# Environment variable names that look like secrets. We strip any var whose
# name matches one of these patterns. The match is case-insensitive.
_SECRET_ENV_PATTERNS: list[re.Pattern[str]] = [
    re.compile(p, re.IGNORECASE)
    for p in [
        r".*_?API[_-]?KEY$",
        r".*_?SECRET$",
        r".*_?PASSWORD$",
        r".*_?PASSWD$",
        r".*_?TOKEN$",
        r".*_?CREDENTIAL$",
        r".*_?PRIVATE[_-]?KEY$",
        r"^API[_-]?KEY$",
        r"^SECRET$",
        r"^TOKEN$",
        r"^PASSWORD$",
        r"^AUTH$",
        r"^CREDENTIAL[S]?$",
        # Known provider keys
        r"^OPENAI_API_KEY$",
        r"^ANTHROPIC_API_KEY$",
        r"^SERPER_API_KEY$",
        r"^TAVILY_API_KEY$",
        r"^TELEGRAM_BOT_TOKEN$",
        r"^SECRET_KEY$",
        # Cloud credentials
        r"^AWS_ACCESS_KEY_ID$",
        r"^AWS_SECRET_ACCESS_KEY$",
        r"^AZURE_CLIENT_SECRET$",
        r"^GOOGLE_APPLICATION_CREDENTIALS$",
    ]
]

# These env vars are safe to keep — they're needed for the subprocess to
# function (Python path, system paths, locale, etc.).
_SAFE_ENV_PREFIXES = (
    "PATH",
    "PYTHON",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "LANG",
    "LC_",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "COMSPEC",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_",
    "OS",
    "COMPUTERNAME",
    "USERNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    "WINDIR",
)


def _looks_secret(name: str) -> bool:
    """True if the env var name matches a secret pattern."""
    for pattern in _SECRET_ENV_PATTERNS:
        if pattern.search(name):
            return True
    return False


def build_sandbox_env(
    *,
    strip_secrets: bool = True,
    extra_allow: list[str] | None = None,
) -> dict[str, str]:
    """Build a sanitized environment dict for subprocess execution.

    When ``strip_secrets`` is True (the default, controlled by
    Settings.sandbox_strip_env), secret-looking env vars are removed. When
    False, the full environment is passed through (legacy behavior).

    ``extra_allow`` lets callers whitelist specific env vars that would
    otherwise be stripped (e.g. a tool-specific API key the subprocess needs).
    """
    if not strip_secrets:
        return dict(os.environ)

    extra_set = {k.upper() for k in (extra_allow or [])}
    safe: dict[str, str] = {}

    for key, value in os.environ.items():
        upper = key.upper()
        # Always allow safe system vars.
        if any(upper.startswith(prefix) for prefix in _SAFE_ENV_PREFIXES):
            safe[key] = value
            continue
        # Explicitly whitelisted by the caller.
        if upper in extra_set:
            safe[key] = value
            continue
        # Strip anything that looks like a secret.
        if _looks_secret(key):
            continue
        # Keep the rest (non-secret, non-system vars).
        safe[key] = value

    stripped_count = len(os.environ) - len(safe)
    if stripped_count > 0:
        log.debug("sandbox.env_stripped", stripped=stripped_count, kept=len(safe))

    return safe
