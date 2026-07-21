"""Symmetric encryption helpers for storing secrets (API keys) at rest."""

from __future__ import annotations

from cryptography.fernet import Fernet, InvalidToken

from app.core.config import get_settings
from app.core.logging import get_logger

log = get_logger(__name__)


def _fernet() -> Fernet:
    settings = get_settings()
    key = settings.secret_key.encode() if settings.secret_key else b""
    if not key or settings.secret_key == "CHANGE_ME":
        log.warning(
            "security.secret_key_not_set",
            hint="Generate with: python -c \"from cryptography.fernet import Fernet; "
            "print(Fernet.generate_key().decode())\"",
        )
    return Fernet(key if key else Fernet.generate_key())


def encrypt(plaintext: str) -> str:
    """Encrypt a plaintext string, return a token string suitable for storage."""
    return _fernet().encrypt(plaintext.encode()).decode()


def decrypt(token: str) -> str:
    """Decrypt a previously encrypted token. Raises ValueError on failure."""
    try:
        return _fernet().decrypt(token.encode()).decode()
    except (InvalidToken, Exception) as exc:
        raise ValueError("Failed to decrypt token") from exc
