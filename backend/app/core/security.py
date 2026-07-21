"""Symmetric encryption helpers for storing secrets (API keys) at rest."""

from __future__ import annotations

import base64
import hashlib
from functools import lru_cache

from cryptography.fernet import Fernet, InvalidToken

from app.core.config import get_settings
from app.core.logging import get_logger

log = get_logger(__name__)


@lru_cache
def _fernet() -> Fernet:
    """Return a process-cached Fernet instance.

    Honors SECRET_KEY when it is a valid Fernet key; otherwise derives a stable
    key from SECRET_KEY so the MVP works out-of-the-box (with a warning that
    secrets won't survive a SECRET_KEY change). The default placeholder
    ``CHANGE_ME`` is replaced so we never silently encrypt with a known value.
    """
    settings = get_settings()
    raw = settings.secret_key or ""
    if not raw or raw == "CHANGE_ME":
        log.warning(
            "security.secret_key_not_set",
            hint="Generate with: python -c \"from cryptography.fernet import Fernet; "
            "print(Fernet.generate_key().decode())\"",
        )
        raw = "placeholder-do-not-use-in-production"

    # If SECRET_KEY is already a valid Fernet key, use it directly.
    try:
        return Fernet(raw.encode())
    except (ValueError, Exception):
        pass

    # Otherwise derive a Fernet key from the secret using a stable hash.
    digest = hashlib.sha256(raw.encode("utf-8")).digest()
    derived = base64.urlsafe_b64encode(digest)
    log.info("security.derived_key", reason="SECRET_KEY is not a valid Fernet key; deriving one")
    return Fernet(derived)


def encrypt(plaintext: str) -> str:
    """Encrypt a plaintext string, return a token string suitable for storage."""
    return _fernet().encrypt(plaintext.encode()).decode()


def decrypt(token: str) -> str:
    """Decrypt a previously encrypted token. Raises ValueError on failure."""
    try:
        return _fernet().decrypt(token.encode()).decode()
    except (InvalidToken, Exception) as exc:
        raise ValueError("Failed to decrypt token") from exc
