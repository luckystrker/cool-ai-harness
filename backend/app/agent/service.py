"""Persistence helpers for conversations and messages.

Keeps SQLModel I/O out of the route handlers and the agent loop. The agent
loop itself stays persistence-agnostic: it works on an in-memory
``list[Message]``; this module knows how to load and store those rows.
"""

from __future__ import annotations

from collections.abc import Sequence

from sqlmodel import Session, select

from app.models import AgentRun, Conversation, RunEvent, User
from app.models import Message as MessageRow
from app.models.run import RUN_STATUS_RUNNING, finish_reason_to_status
from app.providers import Message


def get_or_create_default_user(session: Session) -> User:
    """MVP: return the first user, creating one if necessary.

    Multi-user auth lands in Фаза 6; for now everything belongs to user_id=1.
    """
    user = session.exec(select(User)).first()
    if user is None:
        user = User(username="default", display_name="Default user")
        session.add(user)
        session.commit()
        session.refresh(user)
    return user


def create_conversation(
    session: Session,
    *,
    user_id: int,
    title: str | None = None,
    model: str | None = None,
    working_directory: str | None = None,
    permissions: dict | None = None,
    capability_policy: dict | None = None,
    breakpoints: list[dict] | None = None,
) -> Conversation:
    metadata: dict | None = None
    if breakpoints is not None:
        metadata = {"breakpoints": breakpoints}
    conv = Conversation(
        user_id=user_id,
        title=title,
        model=model,
        working_directory=working_directory,
        permissions=permissions,
        capability_policy=capability_policy,
        metadata_=metadata,
    )
    session.add(conv)
    session.commit()
    session.refresh(conv)
    return conv


def list_conversations(session: Session, *, user_id: int) -> Sequence[Conversation]:
    return session.exec(
        select(Conversation)
        .where(Conversation.user_id == user_id)
        .order_by(Conversation.updated_at.desc())
    ).all()


def get_conversation(session: Session, conv_id: int) -> Conversation | None:
    return session.get(Conversation, conv_id)


def update_conversation(
    session: Session,
    conv_id: int,
    *,
    title: str | None = None,
    model: str | None = None,
    working_directory: str | None = None,
    permissions: dict | None = None,
    capability_policy: dict | None = None,
    breakpoints: list[dict] | None = None,
) -> Conversation | None:
    """Patch updatable fields on a conversation.

    Each argument is optional and uses a sentinel-style guard: ``None`` means
    "leave unchanged". To explicitly clear ``working_directory`` or
    ``permissions``, pass an empty value (``""`` / ``{}`` respectively).
    ``capability_policy`` and ``breakpoints`` follow the same convention.
    """
    conv = session.get(Conversation, conv_id)
    if conv is None:
        return None
    if title is not None:
        conv.title = title
    if model is not None:
        conv.model = model
    if working_directory is not None:
        conv.working_directory = working_directory or None
    if permissions is not None:
        conv.permissions = permissions or None
    if capability_policy is not None:
        conv.capability_policy = capability_policy or None
    if breakpoints is not None:
        meta = dict(conv.metadata_ or {})
        if breakpoints:
            meta["breakpoints"] = breakpoints
        else:
            meta.pop("breakpoints", None)
        conv.metadata_ = meta or None
    session.add(conv)
    session.commit()
    session.refresh(conv)
    return conv


def delete_conversation(session: Session, conv_id: int) -> bool:
    conv = session.get(Conversation, conv_id)
    if conv is None:
        return False
    # Cascade-delete messages first.
    msgs = session.exec(select(MessageRow).where(MessageRow.conversation_id == conv_id)).all()
    for m in msgs:
        session.delete(m)
    session.delete(conv)
    session.commit()
    return True


def load_history(session: Session, conv_id: int) -> list[Message]:
    """Load a conversation's messages in chronological order as provider Messages."""
    rows = session.exec(
        select(MessageRow).where(MessageRow.conversation_id == conv_id).order_by(MessageRow.id)
    ).all()
    return [
        Message(
            role=row.role,
            content=row.content,
            tool_calls=row.tool_calls,
            tool_call_id=row.tool_result.get("tool_call_id") if row.tool_result else None,
            name=row.tool_result.get("name") if row.tool_result else None,
        )
        for row in rows
    ]


def append_message(
    session: Session,
    *,
    conversation_id: int,
    role: str,
    content: str | None = None,
    tool_calls: list[dict] | None = None,
    usage: dict | None = None,
    thinking: str | None = None,
    tool_result: dict | None = None,
) -> MessageRow:
    row = MessageRow(
        conversation_id=conversation_id,
        role=role,
        content=content,
        tool_calls=tool_calls,
        usage=usage,
        thinking=thinking,
        tool_result=tool_result,
    )
    session.add(row)
    session.commit()
    session.refresh(row)
    return row


def list_messages(session: Session, conv_id: int) -> Sequence[MessageRow]:
    return session.exec(
        select(MessageRow).where(MessageRow.conversation_id == conv_id).order_by(MessageRow.id)
    ).all()


# --- Agent runs (Фаза 1.5 — durable runs) ---------------------------------


def create_run(
    session: Session,
    *,
    conversation_id: int,
    user_id: int | None = None,
    model: str | None = None,
    config: dict | None = None,
    status: str = RUN_STATUS_RUNNING,
) -> AgentRun:
    """Create and persist a new AgentRun row."""
    run = AgentRun(
        conversation_id=conversation_id,
        user_id=user_id,
        status=status,
        model=model,
        config=config,
    )
    session.add(run)
    session.commit()
    session.refresh(run)
    return run


def update_run(session: Session, run_id: int, **fields) -> AgentRun | None:
    """Patch arbitrary columns on a run. Returns the run, or None if not found.

    Callers pass only the fields they want to change (e.g. status=...,
    iterations=..., usage=...). ``None`` values are written as-is, so use
    update_run_status/finish_run helpers when a sentinel-free update is needed.
    """
    run = session.get(AgentRun, run_id)
    if run is None:
        return None
    for key, value in fields.items():
        setattr(run, key, value)
    session.add(run)
    session.commit()
    session.refresh(run)
    return run


def update_run_status(session: Session, run_id: int, status: str) -> AgentRun | None:
    return update_run(session, run_id, status=status)


def finish_run(
    session: Session,
    run_id: int,
    *,
    finish_reason: str,
    usage: dict | None = None,
    iterations: int | None = None,
    error: str | None = None,
) -> AgentRun | None:
    """Mark a run terminal: status derived from the finish reason, timestamps set.

    ``iterations`` is only written when provided (it's NOT NULL); ``usage`` and
    ``error`` are written as-is (None clears them, which is valid for both).
    """
    from app.models.base import _utcnow

    fields: dict = {
        "status": finish_reason_to_status(finish_reason),
        "finish_reason": finish_reason,
        "finished_at": _utcnow(),
    }
    if iterations is not None:
        fields["iterations"] = iterations
    # usage/error are nullable, so passing None to clear them is fine.
    fields["usage"] = usage
    if error is not None:
        fields["error"] = error
    return update_run(session, run_id, **fields)


def get_run(session: Session, run_id: int) -> AgentRun | None:
    return session.get(AgentRun, run_id)


def list_runs(session: Session, *, conversation_id: int) -> Sequence[AgentRun]:
    """Runs for a conversation, newest first."""
    return session.exec(
        select(AgentRun)
        .where(AgentRun.conversation_id == conversation_id)
        .order_by(AgentRun.id.desc())
    ).all()


def append_run_events(
    session: Session, *, run_id: int, events: list[tuple[str, dict | None]]
) -> list[RunEvent]:
    """Append a batch of run-event rows in order.

    Each tuple is (kind, payload). ``seq`` is computed from the run's current
    max seq so events stay monotonic across batches. Commits once for the batch
    rather than per-event.
    """
    if not events:
        return []
    current_max = session.exec(
        select(RunEvent.seq).where(RunEvent.run_id == run_id).order_by(RunEvent.seq.desc())
    ).first()
    # ``current_max`` is None when the run has no events yet, else an int (which
    # may legitimately be 0 — so use an explicit None check, not ``or``).
    next_seq = (-1 if current_max is None else current_max) + 1
    rows: list[RunEvent] = []
    for offset, (kind, payload) in enumerate(events):
        row = RunEvent(run_id=run_id, seq=next_seq + offset, kind=kind, payload=payload)
        session.add(row)
        rows.append(row)
    session.commit()
    for row in rows:
        session.refresh(row)
    return rows


def list_run_events(session: Session, *, run_id: int) -> Sequence[RunEvent]:
    """All events for a run, in seq order (the order the loop emitted them)."""
    return session.exec(
        select(RunEvent).where(RunEvent.run_id == run_id).order_by(RunEvent.seq)
    ).all()
