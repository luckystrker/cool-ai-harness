"""Bearer-token authentication for the single-user MVP API.

When ``api_token`` is set in settings, every HTTP request must carry an
``Authorization: Bearer <token>`` header and every WebSocket handshake must
provide the token via ``?token=<token>`` query param. An empty ``api_token``
disables auth (development mode) but logs a loud warning.
"""

from __future__ import annotations

from fastapi import Depends, HTTPException, WebSocket, status
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

from app.core.config import get_settings
from app.core.logging import get_logger

log = get_logger(__name__)

_bearer_scheme = HTTPBearer(auto_error=False)


def _token_configured() -> bool:
    """True when the operator has set a non-empty API token."""
    return bool(get_settings().api_token)


async def require_auth(
    credentials: HTTPAuthorizationCredentials | None = Depends(_bearer_scheme),
) -> None:
    """FastAPI dependency: reject requests without a valid bearer token.

    Skipped entirely when ``api_token`` is empty (dev mode).
    """
    settings = get_settings()
    if not settings.api_token:
        return  # Auth disabled (dev).
    if credentials is None or credentials.credentials != settings.api_token:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid or missing API token",
            headers={"WWW-Authenticate": "Bearer"},
        )


def verify_ws_token(websocket: WebSocket) -> bool:
    """Validate the token for a WebSocket handshake.

    Accepts the token via ``?token=<value>`` query parameter (browsers cannot
    set custom headers on WS upgrades). Returns True if the connection is
    authorized.
    """
    settings = get_settings()
    if not settings.api_token:
        return True  # Auth disabled (dev).
    token = websocket.query_params.get("token", "")
    return token == settings.api_token
