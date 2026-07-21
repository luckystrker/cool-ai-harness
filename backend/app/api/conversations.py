"""Conversation routes: CRUD + SSE streaming for the agent loop."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator

from fastapi import APIRouter, Depends, HTTPException, Request
from sqlmodel import Session
from sse_starlette.sse import EventSourceResponse

from app.agent import AgentConfig, AgentExecutor
from app.agent.service import (
    append_message,
    create_conversation,
    delete_conversation,
    get_conversation,
    get_or_create_default_user,
    list_conversations,
    list_messages,
    load_history,
)
from app.api.schemas import (
    ConversationCreate,
    ConversationDetail,
    ConversationOut,
    MessageOut,
    SendMessageRequest,
)
from app.core.config import get_settings
from app.core.db import get_session
from app.core.logging import get_logger
from app.providers import get_default_provider

log = get_logger(__name__)

router = APIRouter()


def _conv_to_out(conv) -> ConversationOut:
    return ConversationOut(
        id=conv.id,
        user_id=conv.user_id,
        title=conv.title,
        model=conv.model,
        created_at=conv.created_at,
        updated_at=conv.updated_at,
    )


def _msg_to_out(m) -> MessageOut:
    return MessageOut(
        id=m.id,
        conversation_id=m.conversation_id,
        role=m.role,
        content=m.content,
        tool_calls=m.tool_calls,
        usage=m.usage,
        created_at=m.created_at,
    )


# --- CRUD ---


@router.post("/conversations", response_model=ConversationOut)
def post_conversation(
    body: ConversationCreate, session: Session = Depends(get_session)
) -> ConversationOut:
    user = get_or_create_default_user(session)
    settings = get_settings()
    conv = create_conversation(
        session,
        user_id=user.id,
        title=body.title,
        model=body.model or settings.default_model,
    )
    return _conv_to_out(conv)


@router.get("/conversations", response_model=list[ConversationOut])
def get_conversations(session: Session = Depends(get_session)) -> list[ConversationOut]:
    user = get_or_create_default_user(session)
    convs = list_conversations(session, user_id=user.id)
    return [_conv_to_out(c) for c in convs]


@router.get("/conversations/{conv_id}", response_model=ConversationDetail)
def get_conversation_detail(
    conv_id: int, session: Session = Depends(get_session)
) -> ConversationDetail:
    conv = get_conversation(session, conv_id)
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")
    msgs = list_messages(session, conv_id)
    return ConversationDetail(
        id=conv.id,
        user_id=conv.user_id,
        title=conv.title,
        model=conv.model,
        created_at=conv.created_at,
        updated_at=conv.updated_at,
        messages=[_msg_to_out(m) for m in msgs],
    )


@router.delete("/conversations/{conv_id}")
def delete_conversation_route(
    conv_id: int, session: Session = Depends(get_session)
) -> dict:
    if not delete_conversation(session, conv_id):
        raise HTTPException(status_code=404, detail="Conversation not found")
    return {"deleted": conv_id}


# --- streaming chat ---


@router.post("/conversations/{conv_id}/messages")
async def post_message(
    conv_id: int,
    body: SendMessageRequest,
    request: Request,
    session: Session = Depends(get_session),
) -> EventSourceResponse:
    """Append a user message and stream the agent's response as SSE events.

    SSE event payloads are JSON-encoded AgentEvent.to_dict() objects.
    """
    conv = get_conversation(session, conv_id)
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")

    # Persist the user message immediately (before the run starts).
    append_message(
        session,
        conversation_id=conv_id,
        role="user",
        content=body.content,
    )

    settings = get_settings()
    model = body.model or conv.model or settings.default_model
    provider = get_default_provider()

    async def event_stream() -> AsyncIterator[dict]:
        # Load full history (including the user message we just saved) for the loop.
        history = load_history(session, conv_id)
        executor = AgentExecutor(
            provider=provider,
            config=AgentConfig(
                model=model,
                system_prompt=body.system_prompt,
                tool_names=body.tool_names,
            ),
            history=history,
        )

        # Buffer the assistant content / tool calls so we can persist them on finish.
        assistant_content: list[str] = []
        last_tool_calls: list[dict] | None = None

        async for event in executor.stream(None):
            # Persist intermediate tool messages so the DB matches the loop.
            if event.kind == "tool_call_start":
                # Tool result message will be persisted on the tool_result event.
                pass
            elif event.kind == "tool_result":
                payload = event.payload
                append_message(
                    session,
                    conversation_id=conv_id,
                    role="tool",
                    content=payload["result"]["output"],
                    tool_calls=None,
                )
            elif event.kind == "token":
                assistant_content.append(event.payload.get("text", ""))
            elif event.kind == "message":
                last_tool_calls = event.payload.get("tool_calls")
            elif event.kind == "finish":
                # Persist the final assistant message (one row for the turn).
                content = "".join(assistant_content) or None
                if content or last_tool_calls:
                    append_message(
                        session,
                        conversation_id=conv_id,
                        role="assistant",
                        content=content,
                        tool_calls=last_tool_calls,
                        usage=event.payload.get("usage"),
                    )
            yield {"event": event.kind, "data": json.dumps(event.payload, default=str)}

    return EventSourceResponse(event_stream())
