"""Conversation routes: CRUD + SSE streaming for the agent loop."""

from __future__ import annotations

from collections.abc import AsyncIterator

from fastapi import APIRouter, Depends, HTTPException, Request
from sqlmodel import Session, select
from sse_starlette.sse import EventSourceResponse

from app.agent.approvals import approval_registry
from app.agent.permissions import validate as validate_permissions
from app.agent.runners import run_conversation_turn
from app.agent.runs import run_registry
from app.agent.service import (
    append_message,
    create_conversation,
    create_run,
    delete_conversation,
    get_conversation,
    get_or_create_default_user,
    list_conversations,
    list_messages,
    update_conversation,
)
from app.api.schemas import (
    ApprovalAuditOut,
    ConversationCreate,
    ConversationDetail,
    ConversationOut,
    ConversationUpdate,
    MessageOut,
    SendMessageRequest,
    ToolApprovalRequest,
)
from app.core.config import get_settings
from app.core.db import get_session
from app.models import ApprovalAudit
from app.providers import get_default_provider
from app.security.capabilities import validate_policy as validate_capability_policy

router = APIRouter()


def _conv_to_out(conv) -> ConversationOut:
    meta = conv.metadata_ or {}
    return ConversationOut(
        id=conv.id,
        user_id=conv.user_id,
        title=conv.title,
        model=conv.model,
        working_directory=conv.working_directory,
        permissions=conv.permissions,
        capability_policy=conv.capability_policy,
        breakpoints=meta.get("breakpoints"),
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
        thinking=m.thinking,
        tool_result=m.tool_result,
        created_at=m.created_at,
    )


# --- CRUD ---


@router.post("/conversations", response_model=ConversationOut)
def post_conversation(
    body: ConversationCreate, session: Session = Depends(get_session)
) -> ConversationOut:
    if errors := validate_permissions(body.permissions):
        raise HTTPException(status_code=400, detail="; ".join(errors))
    if errors := validate_capability_policy(body.capability_policy):
        raise HTTPException(status_code=400, detail="; ".join(errors))
    user = get_or_create_default_user(session)
    settings = get_settings()
    conv = create_conversation(
        session,
        user_id=user.id,
        title=body.title,
        model=body.model or settings.default_model,
        working_directory=body.working_directory,
        permissions=body.permissions,
        capability_policy=body.capability_policy,
        breakpoints=body.breakpoints,
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
    meta = conv.metadata_ or {}
    return ConversationDetail(
        id=conv.id,
        user_id=conv.user_id,
        title=conv.title,
        model=conv.model,
        working_directory=conv.working_directory,
        permissions=conv.permissions,
        capability_policy=conv.capability_policy,
        breakpoints=meta.get("breakpoints"),
        created_at=conv.created_at,
        updated_at=conv.updated_at,
        messages=[_msg_to_out(m) for m in msgs],
    )


@router.delete("/conversations/{conv_id}")
def delete_conversation_route(conv_id: int, session: Session = Depends(get_session)) -> dict:
    if not delete_conversation(session, conv_id):
        raise HTTPException(status_code=404, detail="Conversation not found")
    return {"deleted": conv_id}


@router.patch("/conversations/{conv_id}", response_model=ConversationOut)
def patch_conversation(
    conv_id: int,
    body: ConversationUpdate,
    session: Session = Depends(get_session),
) -> ConversationOut:
    """Update updatable conversation fields."""
    if errors := validate_permissions(body.permissions):
        raise HTTPException(status_code=400, detail="; ".join(errors))
    if errors := validate_capability_policy(body.capability_policy):
        raise HTTPException(status_code=400, detail="; ".join(errors))
    conv = update_conversation(
        session,
        conv_id,
        title=body.title,
        model=body.model,
        working_directory=body.working_directory,
        permissions=body.permissions,
        capability_policy=body.capability_policy,
        breakpoints=body.breakpoints,
    )
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")
    return _conv_to_out(conv)


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

    # Create a durable run row so this turn is observable, resumable-aware, and
    # cancellable. The run_id flows into the agent loop via the runner.
    run = create_run(session, conversation_id=conv_id, model=model)

    async def event_stream() -> AsyncIterator[dict]:
        try:
            async for event in run_conversation_turn(
                session=session,
                conversation_id=conv_id,
                provider=provider,
                model=model,
                user_input=None,  # already persisted above
                system_prompt=body.system_prompt,
                tool_names=body.tool_names,
                working_directory=conv.working_directory,
                conversation_permissions=conv.permissions,
                conversation_capability_policy=conv.capability_policy,
                conversation_breakpoints=(conv.metadata_ or {}).get("breakpoints"),
                run_id=run.id,
                cancellable=True,
            ):
                yield {"event": event.kind, "data": event.to_dict_json()}
        finally:
            # If the client disconnects (SSE closed), cancel any pending
            # approval and signal the run to stop so the loop doesn't hang or
            # keep working for a dead client.
            approval_registry.cancel_for_conversation(conv_id)
            run_registry.cancel_for_conversation(conv_id)

    return EventSourceResponse(event_stream())


@router.post("/conversations/{conv_id}/tool_calls/{call_id}/approval")
def post_tool_approval(
    conv_id: int,
    call_id: str,
    body: ToolApprovalRequest,
    session: Session = Depends(get_session),
) -> dict:
    """Resolve a pending tool-call approval.

    The agent loop, gated behind an ``ask`` permission, blocks on the approval
    Future registered under ``call_id``. This endpoint resolves it: the loop
    runs the tool if approved, or continues with a denied tool_result if not.
    """
    conv = get_conversation(session, conv_id)
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")
    if not approval_registry.has(call_id):
        raise HTTPException(status_code=404, detail="No pending approval for that call_id")
    resolved = approval_registry.resolve(call_id, body.approved)
    return {"resolved": resolved, "approved": body.approved}


# --- approval audit trail (Фаза 1.5 §2) ---


@router.get("/conversations/{conv_id}/approvals", response_model=list[ApprovalAuditOut])
def get_approval_audits(
    conv_id: int,
    run_id: int | None = None,
    limit: int = 100,
    session: Session = Depends(get_session),
) -> list[ApprovalAuditOut]:
    """List approval audit records for a conversation.

    Optional ``run_id`` query param filters to a single run. Results are
    newest-first, capped at ``limit`` (default 100, max 500).
    """
    conv = get_conversation(session, conv_id)
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")
    limit = min(limit, 500)
    stmt = (
        select(ApprovalAudit)
        .where(ApprovalAudit.conversation_id == conv_id)
        .order_by(ApprovalAudit.id.desc())
        .limit(limit)
    )
    if run_id is not None:
        stmt = stmt.where(ApprovalAudit.run_id == run_id)
    rows = session.exec(stmt).all()
    return [
        ApprovalAuditOut(
            id=r.id,
            conversation_id=r.conversation_id,
            run_id=r.run_id,
            call_id=r.call_id,
            tool_name=r.tool_name,
            arguments=r.arguments,
            approved=r.approved,
            decision_source=r.decision_source,
            decided_by=r.decided_by,
            reason=r.reason,
            is_breakpoint=r.is_breakpoint,
            breakpoint_type=r.breakpoint_type,
            duration_ms=r.duration_ms,
            created_at=r.created_at,
        )
        for r in rows
    ]
