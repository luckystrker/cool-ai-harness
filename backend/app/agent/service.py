"""Persistence helpers for conversations and messages.

Keeps SQLModel I/O out of the route handlers and the agent loop. The agent
loop itself stays persistence-agnostic: it works on an in-memory
``list[Message]``; this module knows how to load and store those rows.
"""

from __future__ import annotations

from collections.abc import Sequence

from sqlmodel import Session, select

from app.models import Conversation, User
from app.models import Message as MessageRow
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
) -> Conversation:
    conv = Conversation(
        user_id=user_id,
        title=title,
        model=model,
        working_directory=working_directory,
        permissions=permissions,
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
) -> Conversation | None:
    """Patch updatable fields on a conversation.

    Each argument is optional and uses a sentinel-style guard: ``None`` means
    "leave unchanged". To explicitly clear ``working_directory`` or
    ``permissions``, pass an empty value (``""`` / ``{}`` respectively).
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
        select(MessageRow)
        .where(MessageRow.conversation_id == conv_id)
        .order_by(MessageRow.id)
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
        select(MessageRow)
        .where(MessageRow.conversation_id == conv_id)
        .order_by(MessageRow.id)
    ).all()
