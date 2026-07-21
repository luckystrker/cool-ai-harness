"""WebSocket endpoint for real-time agent streaming.

Unlike the SSE route (one-shot POST → stream), the WebSocket stays open and
accepts multiple user messages over the same conversation. Clients send
``{"content": "...", "model": "...?"}``; the server streams AgentEvents back
as JSON text frames and finishes each turn with a ``finish`` event.

Closes the conversation model: history is loaded from the DB at turn start,
and assistant/tool messages are persisted at turn end — same runner used by
the SSE route.
"""

from __future__ import annotations

import contextlib

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from app.agent.runners import run_conversation_turn
from app.agent.service import append_message, get_conversation
from app.api.schemas import SendMessageRequest
from app.core.config import get_settings
from app.core.logging import get_logger
from app.providers import get_default_provider

log = get_logger(__name__)

router = APIRouter()


@router.websocket("/ws/chat/{conv_id}")
async def chat_ws(websocket: WebSocket, conv_id: int) -> None:
    """Bidirectional chat stream for a conversation.

    Receive loop: each incoming JSON message is validated as SendMessageRequest,
    persisted as a user row, then drives one agent turn. Events from the turn
    are serialized and sent as text frames. Multiple turns per connection are
    supported; a turn's failure does not close the socket.
    """
    await websocket.accept()

    # We need a DB session independent of FastAPI's request scope (this isn't
    # an HTTP request), so we open one per turn via the sessionmaker.
    from sqlmodel import Session as _Session

    from app.core.db import engine

    try:
        while True:
            raw = await websocket.receive_text()
            try:
                body = SendMessageRequest.model_validate_json(raw)
            except Exception as exc:
                await _send_error(websocket, f"Invalid message: {exc}")
                continue

            with _Session(engine) as session:
                conv = get_conversation(session, conv_id)
                if conv is None:
                    await _send_error(websocket, f"Conversation {conv_id} not found")
                    continue

                # Persist the user's message before the run.
                append_message(
                    session,
                    conversation_id=conv_id,
                    role="user",
                    content=body.content,
                )

                settings = get_settings()
                model = body.model or conv.model or settings.default_model
                provider = get_default_provider()

                try:
                    async for event in run_conversation_turn(
                        session=session,
                        conversation_id=conv_id,
                        provider=provider,
                        model=model,
                        user_input=None,  # already persisted
                        system_prompt=body.system_prompt,
                        tool_names=body.tool_names,
                    ):
                        await websocket.send_text(event.to_dict_json())
                except Exception as exc:
                    log.error("ws.turn_failed", conv_id=conv_id, error=str(exc))
                    await _send_error(websocket, f"Turn failed: {exc}")
    except WebSocketDisconnect:
        log.info("ws.disconnected", conv_id=conv_id)
    except Exception as exc:
        log.error("ws.fatal", conv_id=conv_id, error=str(exc))
        with contextlib.suppress(Exception):
            await _send_error(websocket, f"Fatal: {exc}")


async def _send_error(websocket: WebSocket, message: str) -> None:
    from app.agent.events import AgentEvent

    await websocket.send_text(AgentEvent.error(message).to_dict_json())
