"""Post-run memory extraction hook (Фаза 3a — auto-extraction).

Called after a main agent run finishes (chat turns and scheduled task runs —
not subagents). Guards on settings, debounces by a per-conversation message
pointer, and delegates to the existing LLM extractor.

The extraction runs as a fire-and-forget asyncio task with its own DB session:
it must never block or fail the run that triggered it.
"""

from __future__ import annotations

import asyncio
from datetime import UTC, datetime

from sqlmodel import Session, func, select

from app.core.db import engine
from app.core.logging import get_logger
from app.memory.extractor import (
    MIN_TOOL_CALLS_FOR_EXTRACTION,
    extract_memories_from_conversation,
)

log = get_logger(__name__)

# WorkingMemory.state key holding the last extracted message id.
_EXTRACTION_POINTER_KEY = "last_extraction_message_id"


def _last_extraction_pointer(session: Session, conversation_id: int) -> int | None:
    from app.memory.service import get_working_memory

    wm = get_working_memory(session, conversation_id)
    state = wm.state if wm is not None else {}
    ptr = state.get(_EXTRACTION_POINTER_KEY)
    return ptr if isinstance(ptr, int) else None


def _set_extraction_pointer(session: Session, conversation_id: int, message_id: int) -> None:
    from app.memory.service import get_or_create_working_memory

    wm = get_or_create_working_memory(session, conversation_id)
    state = dict(wm.state or {})
    state[_EXTRACTION_POINTER_KEY] = message_id
    wm.state = state
    wm.updated_at = datetime.now(UTC)
    session.add(wm)
    session.commit()


def _new_activity_counts(
    session: Session, conversation_id: int, pointer: int | None
) -> tuple[int, int]:
    """Count (new_messages, new_tool_calls) in the conversation since the pointer."""
    from app.models import Message as MessageRow

    stmt = select(MessageRow).where(MessageRow.conversation_id == conversation_id)
    if pointer is not None:
        stmt = stmt.where(MessageRow.id > pointer)  # type: ignore[operator]
    rows = session.exec(stmt).all()
    messages = len(rows)
    tool_calls = sum(1 for m in rows if m.role == "tool" and (m.tool_result or {}).get("name"))
    return messages, tool_calls


def _should_extract(messages: int, tool_calls: int, min_interval: int) -> bool:
    """Debounce gate: enough new messages, or a tool-heavy turn."""
    if min_interval <= 0:
        return messages > 0
    return messages >= max(min_interval, 6) or tool_calls >= MIN_TOOL_CALLS_FOR_EXTRACTION


async def maybe_extract_after_run(
    *,
    conversation_id: int,
    run_id: int | None,
    provider,
    model: str,
) -> bool:
    """Run extraction for a finished conversation turn. Best-effort, returns
    True when extraction was actually attempted (used by tests)."""
    from app.core.config import get_settings

    settings = get_settings()
    if not settings.memory_enabled or not settings.memory_extraction_enabled:
        return False
    if conversation_id is None:
        return False

    try:
        with Session(engine) as session:
            pointer = _last_extraction_pointer(session, conversation_id)
            messages, tool_calls = _new_activity_counts(session, conversation_id, pointer)
            if not _should_extract(messages, tool_calls, settings.memory_extraction_min_interval):
                return False

            from app.agent.service import (
                get_or_create_default_user,
                load_history,
                resolve_default_model,
            )

            user = get_or_create_default_user(session)
            assert user.id is not None
            history = load_history(session, conversation_id)
            if len(history) < 6:
                return False

            result = await extract_memories_from_conversation(
                session,
                provider=provider,
                model=model or resolve_default_model(session) or "",
                user_id=user.id,
                conversation_id=conversation_id,
                run_id=run_id,
                messages=history,
            )

            # Advance the pointer whenever extraction was attempted (success or
            # not): a skipped/llm_error/parse_error result must not re-trigger a
            # full extraction on every subsequent turn.
            from app.models import Message as MessageRow

            last_id = session.exec(
                select(func.max(MessageRow.id)).where(
                    MessageRow.conversation_id == conversation_id
                )
            ).one()
            if last_id is not None:
                _set_extraction_pointer(session, conversation_id, int(last_id))

            return not result.get("skipped")
    except Exception as exc:
        log.warning(
            "memory.auto_extraction_failed", conversation_id=conversation_id, error=str(exc)
        )
        return False


def schedule_after_run(*, conversation_id: int, run_id: int | None, provider, model: str) -> None:
    """Fire-and-forget wrapper: spawns ``maybe_extract_after_run`` on the loop."""
    loop = asyncio.get_running_loop()
    task = loop.create_task(
        maybe_extract_after_run(
            conversation_id=conversation_id,
            run_id=run_id,
            provider=provider,
            model=model,
        )
    )

    def _on_done(t: asyncio.Task) -> None:
        exc = t.exception() if not t.cancelled() else None
        if exc is not None:
            log.warning("memory.auto_extraction_task_error", error=str(exc))

    task.add_done_callback(_on_done)
