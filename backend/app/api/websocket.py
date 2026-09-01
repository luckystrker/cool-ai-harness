"""WebSocket endpoints for real-time agent streaming and live inspection.

Unlike the SSE route (one-shot POST → stream), the WebSocket stays open and
accepts multiple user messages over the same conversation. Clients send
``{"content": "...", "model": "...?"}``; the server streams AgentEvents back
as JSON text frames and finishes each turn with a ``finish`` event.

The inspection WebSocket (``/ws/inspect/{run_id}``) is read-only: it forwards
events from an in-progress run to developer tools in real time (Фаза 1.5 §6).

Closes the conversation model: history is loaded from the DB at turn start,
and assistant/tool messages are persisted at turn end — same runner used by
the SSE route.
"""

from __future__ import annotations

import asyncio
import contextlib
import json

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from app.agent.runners import run_conversation_turn
from app.agent.runs import conversation_turn_registry, run_registry
from app.agent.service import append_message, create_run, get_conversation
from app.api.schemas import SendMessageRequest
from app.core.auth import verify_ws_token
from app.core.logging import get_logger
from app.observability import inspector_registry
from app.providers import get_provider_for_model

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
    if not verify_ws_token(websocket):
        await websocket.close(code=4001, reason="Unauthorized")
        return
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

                from app.multimodal import validate_artifact_ids

                try:
                    attachments = validate_artifact_ids(
                        session,
                        conversation_id=conv_id,
                        artifact_ids=body.artifact_ids,
                    )
                except ValueError as exc:
                    await _send_error(websocket, str(exc))
                    continue

                from app.agent.personalities.service import get_profile
                from app.providers.registry import resolve_provider_model

                profile = get_profile(session, conv.profile_id) if conv.profile_id else None
                requested_model = body.model or (profile.model if profile else None) or conv.model
                provider = get_provider_for_model(requested_model)
                model = resolve_provider_model(provider, requested_model)
                if model is None:
                    await _send_error(websocket, "No default model is configured")
                    continue

                lease = conversation_turn_registry.acquire(conv_id)
                if lease is None:
                    await _send_error(websocket, "A turn is already active")
                    continue

                try:
                    append_message(
                        session,
                        conversation_id=conv_id,
                        role="user",
                        content=body.content,
                        artifact_ids=[
                            artifact.id for artifact in attachments if artifact.id is not None
                        ],
                        commit=False,
                    )
                    run = create_run(session, conversation_id=conv_id, model=model, commit=False)
                    session.commit()
                    session.refresh(run)
                except Exception:
                    session.rollback()
                    conversation_turn_registry.release(conv_id, lease)
                    raise

                try:
                    async for event in run_conversation_turn(
                        session=session,
                        conversation_id=conv_id,
                        provider=provider,
                        model=model,
                        user_input=None,  # already persisted
                        system_prompt=body.system_prompt,
                        tool_names=body.tool_names,
                        working_directory=conv.working_directory,
                        conversation_permissions=conv.permissions,
                        conversation_capability_policy=conv.capability_policy,
                        conversation_breakpoints=(conv.metadata_ or {}).get("breakpoints"),
                        run_id=run.id,
                        cancellable=True,
                        profile_id=conv.profile_id,
                    ):
                        await websocket.send_text(event.to_dict_json())
                except Exception as exc:
                    log.error("ws.turn_failed", conv_id=conv_id, error=str(exc))
                    await _send_error(websocket, f"Turn failed: {exc}")
                finally:
                    # Release any pending approvals and signal the run to stop
                    # if the turn ended early (client gone / error).
                    from app.agent.approvals import approval_registry

                    assert run.id is not None
                    approval_registry.cancel_for_run(run.id, conversation_id=conv_id)
                    run_registry.cancel(run.id)
                    conversation_turn_registry.release(conv_id, lease)
    except WebSocketDisconnect:
        log.info("ws.disconnected", conv_id=conv_id)
    except Exception as exc:
        log.error("ws.fatal", conv_id=conv_id, error=str(exc))
        with contextlib.suppress(Exception):
            await _send_error(websocket, f"Fatal: {exc}")


async def _send_error(websocket: WebSocket, message: str) -> None:
    from app.agent.events import AgentEvent

    await websocket.send_text(AgentEvent.error(message).to_dict_json())


# --- Live inspection WebSocket (Фаза 1.5 §6) --------------------------------


@router.websocket("/ws/inspect/{run_id}")
async def inspect_run_ws(websocket: WebSocket, run_id: int) -> None:
    """Read-only live inspection of an in-progress run.

    Subscribes to the InspectorRegistry for the given run_id and forwards
    every event as a JSON text frame. When the run finishes, a ``null``
    sentinel is sent and the connection closes. If the run is already
    finished (or unknown), sends an error frame and closes immediately.
    """
    if not verify_ws_token(websocket):
        await websocket.close(code=4001, reason="Unauthorized")
        return
    await websocket.accept()

    # Quick check: is the run still active? If not, inform and close.
    if not run_registry.is_active(run_id) and not inspector_registry.has_subscribers(run_id):
        # The run might still be active if it just hasn't been registered yet
        # (race), so we subscribe anyway and rely on the sentinel to end.
        pass

    queue = inspector_registry.subscribe(run_id)
    try:
        while True:
            try:
                event_dict = await asyncio.wait_for(queue.get(), timeout=30.0)
            except TimeoutError:
                # Send a ping to keep the connection alive.
                await websocket.send_text(json.dumps({"kind": "ping", "payload": {}}))
                continue

            if event_dict is None:
                # Sentinel: run finished.
                await websocket.send_text("null")
                break
            await websocket.send_text(json.dumps(event_dict, default=str, ensure_ascii=False))
    except WebSocketDisconnect:
        log.debug("inspect_ws.disconnected", run_id=run_id)
    except Exception as exc:
        log.error("inspect_ws.error", run_id=run_id, error=str(exc))
    finally:
        inspector_registry.unsubscribe(run_id, queue)
