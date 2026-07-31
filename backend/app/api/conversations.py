"""Conversation routes: CRUD + SSE streaming for the agent loop."""

from __future__ import annotations

from collections.abc import AsyncIterator

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel
from sqlmodel import Session, select
from sse_starlette.sse import EventSourceResponse

from app.agent.approvals import approval_registry
from app.agent.permissions import validate as validate_permissions
from app.agent.planning import PLANNING_SYSTEM_PROMPT, create_plan, extract_plan_from_response
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
from app.core.db import get_session
from app.models import ApprovalAudit, Conversation
from app.providers import get_provider_for_model
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
        profile_id=conv.profile_id,
        tags=conv.tags or [],
        folder=conv.folder,
        is_pinned=conv.is_pinned,
        is_archived=conv.is_archived,
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
        model=m.model,
        duration_ms=m.duration_ms,
    )


def _resolve_default_model(session: Session) -> str | None:
    """Default model for a new conversation: the default provider's first chat
    model (falling back to its legacy default_model column).

    Picks the row marked ``is_default``; if none, the first active non-fallback
    row. Returns None when no provider / no models are configured.
    """
    from sqlmodel import select

    from app.models import Provider as ProviderRow

    rows = session.exec(
        select(ProviderRow)
        .where(ProviderRow.user_id == 1)
        .where(ProviderRow.is_active == True)  # noqa: E712
        .order_by(ProviderRow.id)
    ).all()
    pool = [r for r in rows if r.is_default and not r.is_fallback] or [
        r for r in rows if not r.is_fallback
    ]
    if not pool:
        return None
    row = pool[0]
    chat_models = list(row.chat_models or [])
    return (chat_models[0] if chat_models else None) or row.default_model


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
    # Seed the conversation's model from the default provider when the caller
    # didn't name one, so a freshly created chat already has a working model.
    model = body.model or _resolve_default_model(session)
    conv = create_conversation(
        session,
        user_id=user.id,
        title=body.title,
        model=model,
        working_directory=body.working_directory,
        permissions=body.permissions,
        capability_policy=body.capability_policy,
        breakpoints=body.breakpoints,
        profile_id=body.profile_id,
    )
    return _conv_to_out(conv)


@router.get("/conversations", response_model=list[ConversationOut])
def get_conversations(session: Session = Depends(get_session)) -> list[ConversationOut]:
    user = get_or_create_default_user(session)
    convs = list_conversations(session, user_id=user.id)
    # Hide conversations flagged as test artifacts (metadata_.is_test). These
    # are rows left behind by old test runs that pre-date the isolated test
    # database; they should never clutter the real chat list.
    return [
        _conv_to_out(c)
        for c in convs
        if not (c.metadata_ or {}).get("is_test")
    ]


@router.get("/conversations/{conv_id}", response_model=ConversationDetail)
def get_conversation_detail(
    conv_id: int, session: Session = Depends(get_session)
) -> ConversationDetail:
    conv = get_conversation(session, conv_id)
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")
    msgs = list_messages(session, conv_id)
    meta = conv.metadata_ or {}
    # Compaction state: the rolling summary + cutoff let the UI collapse the
    # already-compacted messages behind the summary text.
    from app.memory.service import get_working_memory

    wm = get_working_memory(session, conv_id)
    compact_summary = wm.summary if wm is not None and wm.summary else None
    compact_cutoff = (
        wm.summary_up_to_message_id if compact_summary is not None and wm is not None else None
    )
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
        compact_summary=compact_summary,
        compact_up_to_message_id=compact_cutoff,
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
        profile_id=body.profile_id,
        tags=body.tags,
        folder=body.folder,
        is_pinned=body.is_pinned,
        is_archived=body.is_archived,
    )
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")
    return _conv_to_out(conv)


@router.post("/conversations/{conv_id}/compact")
async def compact_conversation(
    conv_id: int, session: Session = Depends(get_session)
) -> dict:
    """Compact the conversation context by summarizing older messages.

    Triggers the working memory summarization: older messages are compressed
    into a rolling summary stored in WorkingMemory, reducing the context size
    for future turns. ``load_history`` skips messages covered by the summary,
    so the next agent turn sees only the recent messages plus the summary
    (injected via the memory context block).
    """
    from app.core.config import get_settings
    from app.memory.service import get_working_memory, update_working_memory_summary
    from app.providers import Message as ProviderMessage

    settings = get_settings()
    if not settings.memory_enabled:
        return {"status": "skipped", "reason": "Memory subsystem is disabled"}

    conv = get_conversation(session, conv_id)
    if conv is None:
        raise HTTPException(status_code=404, detail="Conversation not found")

    msgs = list_messages(session, conv_id)

    # Only compact if there are enough messages.
    threshold = settings.memory_summary_threshold_messages
    if len(msgs) < threshold:
        return {
            "status": "skipped",
            "reason": f"Too few messages ({len(msgs)} < {threshold})",
            "message_count": len(msgs),
        }

    # Build transcript of older messages (all but the last 10). When a
    # previous summary exists, only messages after its cutoff are new —
    # re-summarizing the already-compacted rows would waste tokens.
    wm = get_working_memory(session, conv_id)
    prev_cutoff = (
        wm.summary_up_to_message_id
        if wm is not None and wm.summary and wm.summary_up_to_message_id is not None
        else None
    )
    keep_recent = 10
    older_msgs = msgs[:-keep_recent] if len(msgs) > keep_recent else []
    new_msgs = [
        m for m in older_msgs if prev_cutoff is None or (m.id is not None and m.id > prev_cutoff)
    ]
    if not new_msgs:
        return {"status": "skipped", "reason": "No new messages to compact"}

    # Build a transcript for summarization. Roll the previous summary forward
    # so the new one covers the whole compacted prefix.
    transcript_lines = []
    if wm is not None and wm.summary:
        transcript_lines.append(f"[Previous summary of the earlier conversation]\n{wm.summary}")
    for m in new_msgs:
        role = m.role
        content = m.content or ""
        if len(content) > 300:
            content = content[:300] + "..."
        transcript_lines.append(f"{role}: {content}")
    transcript = "\n".join(transcript_lines)

    # Summarize using the LLM.
    model = conv.model or _resolve_default_model(session)
    if model is None:
        raise HTTPException(status_code=400, detail="No model configured")

    provider = get_provider_for_model(model)
    summary_model = settings.memory_summary_model or model

    summarization_prompt = (
        "Summarize the following conversation transcript into a concise summary "
        "that preserves key decisions, facts, and context. Focus on information "
        "that would be useful for continuing the conversation. Keep it under 500 words.\n\n"
        f"Transcript:\n{transcript}"
    )

    try:
        result = await provider.chat_completion(
            [ProviderMessage(role="user", content=summarization_prompt)],
            model=summary_model,
            temperature=0.3,
            max_tokens=1000,
        )
        summary = result.content or ""
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"Summarization failed: {exc}") from exc

    # Store the summary in working memory. The cutoff covers the whole
    # compacted prefix (previous + newly compacted messages).
    last_compacted_msg_id = older_msgs[-1].id if older_msgs else None
    update_working_memory_summary(
        session, conv_id, summary, up_to_message_id=last_compacted_msg_id
    )

    return {
        "status": "compacted",
        "messages_compacted": len(new_msgs),
        "messages_kept": keep_recent,
        "summary_length": len(summary),
    }


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
    When ``plan_mode`` is True, the agent generates a structured plan instead
    of executing directly (Фаза 2 §1 Planning Mode).
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

    model = body.model or conv.model
    provider = get_provider_for_model(model)

    # Create a durable run row so this turn is observable, resumable-aware, and
    # cancellable. The run_id flows into the agent loop via the runner.
    run = create_run(session, conversation_id=conv_id, model=model)

    # --- Planning Mode (Фаза 2 §1) ---
    if body.plan_mode:
        return EventSourceResponse(
            _plan_generation_stream(session, conv_id, run.id, provider, model or "", body.content, conv)  # type: ignore[arg-type]
        )

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
                profile_id=conv.profile_id,
            ):
                yield {"event": event.kind, "data": event.to_dict_json()}
        finally:
            # If the client disconnects (SSE closed), cancel any pending
            # approval and signal the run to stop so the loop doesn't hang or
            # keep working for a dead client.
            approval_registry.cancel_for_conversation(conv_id)
            run_registry.cancel_for_conversation(conv_id)

    return EventSourceResponse(event_stream())


async def _plan_generation_stream(
    session, conv_id: int, run_id: int, provider, model: str, user_input: str, conv
) -> AsyncIterator[dict]:
    """SSE stream for plan generation: runs the full agent loop with planning prompt.

    The agent researches using tools, then outputs a ```plan ... ``` block.
    After the loop finishes, the plan is extracted, persisted, and emitted.
    """
    from app.agent.events import AgentEvent
    from app.agent.service import update_run
    from app.models.run import RUN_STATUS_AWAITING_APPROVAL

    last_content = ""

    try:
        async for event in run_conversation_turn(
            session=session,
            conversation_id=conv_id,
            provider=provider,
            model=model,
            user_input=None,  # already persisted above
            system_prompt=PLANNING_SYSTEM_PROMPT,
            working_directory=conv.working_directory,
            conversation_permissions=conv.permissions,
            conversation_capability_policy=conv.capability_policy,
            conversation_breakpoints=(conv.metadata_ or {}).get("breakpoints"),
            run_id=run_id,
            cancellable=True,
        ):
            # Capture the last assistant message content for plan extraction.
            if event.kind == "message":
                last_content = event.payload.get("content") or ""
            yield {"event": event.kind, "data": event.to_dict_json()}

        # After the loop finishes, try to extract a plan from the final response.
        plan_data = extract_plan_from_response(last_content)
        if plan_data:
            plan = create_plan(
                session,
                conversation_id=conv_id,
                run_id=run_id,
                title=plan_data.get("title"),
                steps=plan_data["steps"],
            )
            # Emit the plan_generated event so the frontend shows the PlanCard.
            plan_event = AgentEvent.plan_generated(
                plan_id=plan.id,  # type: ignore[arg-type]
                title=plan.title,
                steps=plan_data["steps"],
            )
            yield {"event": plan_event.kind, "data": plan_event.to_dict_json()}
            # Mark the run as awaiting approval (user must approve the plan).
            update_run(session, run_id, status=RUN_STATUS_AWAITING_APPROVAL)
    finally:
        approval_registry.cancel_for_conversation(conv_id)
        run_registry.cancel_for_conversation(conv_id)


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


# --- Conversation search & bulk operations (Фаза 3a §4) ---


@router.get("/conversations/search/content", response_model=list[ConversationOut])
def search_conversation_content(
    q: str,
    limit: int = 20,
    session: Session = Depends(get_session),
) -> list[ConversationOut]:
    """Search across all conversation message content."""
    from sqlmodel import col

    from app.models import Message as MessageRow

    # Find conversation IDs that have matching messages.
    msg_stmt = (
        select(MessageRow.conversation_id)
        .where(col(MessageRow.content).contains(q))
        .distinct()
        .limit(limit)
    )
    conv_ids = list(session.exec(msg_stmt).all())
    if not conv_ids:
        return []
    convs = session.exec(
        select(Conversation).where(col(Conversation.id).in_(conv_ids))
    ).all()
    return [_conv_to_out(c) for c in convs]


class BulkActionRequest(BaseModel):
    """Bulk operation request body."""

    conversation_ids: list[int]
    action: str  # "archive" | "unarchive" | "pin" | "unpin" | "delete" | "move"
    folder: str | None = None  # for "move" action


@router.post("/conversations/bulk")
def post_conversations_bulk(
    body: BulkActionRequest, session: Session = Depends(get_session)
) -> dict:
    """Perform a bulk operation on multiple conversations."""
    affected = 0
    for conv_id in body.conversation_ids:
        conv = session.get(Conversation, conv_id)
        if conv is None:
            continue
        if body.action == "archive":
            conv.is_archived = True
        elif body.action == "unarchive":
            conv.is_archived = False
        elif body.action == "pin":
            conv.is_pinned = True
        elif body.action == "unpin":
            conv.is_pinned = False
        elif body.action == "delete":
            delete_conversation(session, conv_id)
            affected += 1
            continue
        elif body.action == "move" and body.folder is not None:
            conv.folder = body.folder or None
        else:
            continue
        session.add(conv)
        affected += 1
    session.commit()
    return {"affected": affected, "action": body.action}
