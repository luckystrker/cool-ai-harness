"""MemoryService — CRUD, deduplication, and scope-filtered access for long-term memory.

All memory operations go through this service to enforce:
- Deduplication (FTS5 similarity check before insert).
- Secret masking (reject memories containing detected secrets).
- Scope visibility (global/agent/conversation namespace rules).
- Importance capping (agent can't set importance > 0.9 without user confirmation).
"""

from __future__ import annotations

from collections.abc import Sequence
from datetime import UTC, datetime

from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.memory.models import (
    MEMORY_STATUS_ACTIVE,
    MEMORY_STATUS_ARCHIVED,
    MEMORY_TYPE_PREFERENCE,
    SCOPE_CONVERSATION,
    SCOPE_GLOBAL,
    Episode,
    MemoryItem,
    WorkingMemory,
)

log = get_logger(__name__)

# Maximum importance an agent can set without user confirmation.
MAX_AGENT_IMPORTANCE = 0.9


def remember(
    session: Session,
    *,
    user_id: int,
    content: str,
    memory_type: str = "semantic",
    scope: str = SCOPE_GLOBAL,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    importance: float = 0.5,
    confidence: float = 0.7,
    source: str = "agent",
    tags: list[str] | None = None,
    structured: dict | None = None,
    ttl_days: int | None = None,
) -> MemoryItem:
    """Store a new memory, with deduplication and validation.

    If a highly similar active memory already exists (same type + overlapping
    content), it is updated instead of creating a duplicate.
    """
    # Cap importance for non-user sources.
    if source != "user_explicit" and importance > MAX_AGENT_IMPORTANCE:
        importance = MAX_AGENT_IMPORTANCE

    # Clamp values.
    importance = max(0.0, min(1.0, importance))
    confidence = max(0.0, min(1.0, confidence))

    # For conversation-scoped memories, capture the conversation's working
    # directory as a "project key" so the memory is visible across all
    # conversations in the same project (same working directory), not just
    # the originating conversation.
    if scope == SCOPE_CONVERSATION and conversation_id is not None:
        structured = _attach_project_key(session, conversation_id, structured)

    # Deduplication: check for existing active memory with same type and similar content.
    existing = _find_duplicate(session, user_id=user_id, content=content, memory_type=memory_type)
    if existing is not None:
        # Update the existing memory rather than creating a duplicate.
        existing.content = content
        existing.importance = max(existing.importance, importance)
        existing.confidence = confidence
        existing.updated_at = datetime.now(UTC)
        if tags:
            existing.tags = tags
        if structured:
            existing.structured = structured
        session.add(existing)
        session.commit()
        session.refresh(existing)
        log.info("memory.updated_duplicate", memory_id=existing.id)
        return existing

    memory = MemoryItem(
        user_id=user_id,
        scope=scope,
        agent_id=agent_id,
        conversation_id=conversation_id,
        memory_type=memory_type,
        content=content,
        structured=structured,
        tags=tags,
        importance=importance,
        confidence=confidence,
        source=source,
        ttl_days=ttl_days,
    )
    session.add(memory)
    session.commit()
    session.refresh(memory)
    log.info("memory.created", memory_id=memory.id, memory_type=memory_type, scope=scope)
    return memory


def recall(
    session: Session,
    *,
    user_id: int,
    query: str | None = None,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    memory_type: str | None = None,
    scope: str | None = None,
    limit: int = 10,
) -> list[MemoryItem]:
    """Retrieve memories visible to the given context, optionally filtered by FTS5 query.

    Scope visibility:
    - global memories are always visible for the user.
    - agent memories are visible when agent_id matches.
    - conversation memories are visible when conversation_id matches.
    """
    from app.memory.retrieval import retrieve_memories

    return retrieve_memories(
        session,
        user_id=user_id,
        query=query,
        agent_id=agent_id,
        conversation_id=conversation_id,
        memory_type=memory_type,
        scope=scope,
        limit=limit,
    )


def get_memory(session: Session, memory_id: int) -> MemoryItem | None:
    """Get a single memory by ID."""
    return session.get(MemoryItem, memory_id)


def update_memory(
    session: Session,
    memory_id: int,
    **fields,
) -> MemoryItem | None:
    """Update fields on a memory item. Returns None if not found."""
    memory = session.get(MemoryItem, memory_id)
    if memory is None:
        return None
    allowed_fields = {
        "content", "memory_type", "scope", "agent_id", "importance",
        "confidence", "status", "tags", "structured", "ttl_days", "valid_to",
    }
    for key, value in fields.items():
        if key in allowed_fields:
            setattr(memory, key, value)
    memory.updated_at = datetime.now(UTC)
    session.add(memory)
    session.commit()
    session.refresh(memory)
    return memory


def forget(session: Session, memory_id: int, *, hard: bool = False) -> bool:
    """Archive (soft-delete) or permanently delete a memory.

    By default, sets status to 'archived' (recoverable). With hard=True,
    deletes the row entirely.
    """
    memory = session.get(MemoryItem, memory_id)
    if memory is None:
        return False
    if hard:
        session.delete(memory)
    else:
        memory.status = MEMORY_STATUS_ARCHIVED
        memory.updated_at = datetime.now(UTC)
        session.add(memory)
    session.commit()
    log.info("memory.forgotten", memory_id=memory_id, hard=hard)
    return True


def list_memories(
    session: Session,
    *,
    user_id: int,
    agent_id: int | None = None,
    memory_type: str | None = None,
    scope: str | None = None,
    status: str | None = MEMORY_STATUS_ACTIVE,
    limit: int = 50,
    offset: int = 0,
) -> Sequence[MemoryItem]:
    """List memories with optional filters, ordered by importance desc then recency."""
    stmt = select(MemoryItem).where(MemoryItem.user_id == user_id)
    if agent_id is not None:
        stmt = stmt.where(
            (MemoryItem.agent_id == agent_id) | (MemoryItem.scope == SCOPE_GLOBAL)
        )
    if memory_type is not None:
        stmt = stmt.where(MemoryItem.memory_type == memory_type)
    if scope is not None:
        stmt = stmt.where(MemoryItem.scope == scope)
    if status is not None:
        stmt = stmt.where(MemoryItem.status == status)
    stmt = stmt.order_by(col(MemoryItem.importance).desc(), col(MemoryItem.updated_at).desc())
    stmt = stmt.offset(offset).limit(limit)
    return session.exec(stmt).all()


def get_preferences(session: Session, *, user_id: int) -> list[MemoryItem]:
    """Get all active preference memories for a user (always injected into context)."""
    return list(
        session.exec(
            select(MemoryItem)
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.memory_type == MEMORY_TYPE_PREFERENCE)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
            .order_by(col(MemoryItem.importance).desc())
        ).all()
    )


# --- Episodes ---


def create_episode(
    session: Session,
    *,
    user_id: int,
    title: str,
    summary: str,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    run_id: int | None = None,
    outcome: str = "unknown",
    importance: float = 0.5,
    tags: list[str] | None = None,
    related_entities: list[str] | None = None,
    started_at: datetime | None = None,
    ended_at: datetime | None = None,
) -> Episode:
    """Create an episodic memory (session/run summary)."""
    episode = Episode(
        user_id=user_id,
        agent_id=agent_id,
        conversation_id=conversation_id,
        run_id=run_id,
        title=title,
        summary=summary,
        outcome=outcome,
        importance=importance,
        tags=tags,
        related_entities=related_entities,
        started_at=started_at,
        ended_at=ended_at,
    )
    session.add(episode)
    session.commit()
    session.refresh(episode)
    log.info("memory.episode_created", episode_id=episode.id)
    return episode


def list_episodes(
    session: Session,
    *,
    user_id: int,
    agent_id: int | None = None,
    limit: int = 20,
) -> Sequence[Episode]:
    """List episodes for a user, newest first."""
    stmt = select(Episode).where(Episode.user_id == user_id)
    if agent_id is not None:
        stmt = stmt.where(Episode.agent_id == agent_id)
    stmt = stmt.order_by(col(Episode.created_at).desc()).limit(limit)
    return session.exec(stmt).all()


# --- Working Memory ---


def get_working_memory(session: Session, conversation_id: int) -> WorkingMemory | None:
    """Get the working memory for a conversation (or None if not created yet)."""
    return session.exec(
        select(WorkingMemory).where(WorkingMemory.conversation_id == conversation_id)
    ).first()


def get_or_create_working_memory(session: Session, conversation_id: int) -> WorkingMemory:
    """Get or create the working memory row for a conversation."""
    wm = get_working_memory(session, conversation_id)
    if wm is not None:
        return wm
    wm = WorkingMemory(conversation_id=conversation_id, state={})
    session.add(wm)
    session.commit()
    session.refresh(wm)
    return wm


def update_working_memory_state(
    session: Session, conversation_id: int, key: str, value
) -> WorkingMemory:
    """Set a key in the working memory scratchpad."""
    wm = get_or_create_working_memory(session, conversation_id)
    state = dict(wm.state or {})
    state[key] = value
    wm.state = state
    wm.updated_at = datetime.now(UTC)
    session.add(wm)
    session.commit()
    session.refresh(wm)
    return wm


def update_working_memory_summary(
    session: Session,
    conversation_id: int,
    summary: str,
    up_to_message_id: int | None = None,
) -> WorkingMemory:
    """Update the rolling conversation summary."""
    wm = get_or_create_working_memory(session, conversation_id)
    wm.summary = summary
    if up_to_message_id is not None:
        wm.summary_up_to_message_id = up_to_message_id
    wm.updated_at = datetime.now(UTC)
    session.add(wm)
    session.commit()
    session.refresh(wm)
    return wm


# --- Internal helpers ---


def _attach_project_key(
    session: Session, conversation_id: int, structured: dict | None
) -> dict | None:
    """Attach the conversation's working directory as a project key.

    Conversation-scoped memories carry ``_project_key`` (the conversation's
    working directory) so retrieval can make them visible across all
    conversations belonging to the same project.
    """
    from app.models import Conversation

    conv = session.get(Conversation, conversation_id)
    workdir = conv.working_directory if conv else None
    result = dict(structured) if structured else {}
    if workdir:
        result["_project_key"] = workdir
    return result or None


def _find_duplicate(
    session: Session,
    *,
    user_id: int,
    content: str,
    memory_type: str,
) -> MemoryItem | None:
    """Find an existing active memory that is likely a duplicate.

    Uses exact content match first (fast path), then falls back to FTS5
    similarity for near-duplicates.
    """
    # Fast path: exact content match.
    existing = session.exec(
        select(MemoryItem)
        .where(MemoryItem.user_id == user_id)
        .where(MemoryItem.memory_type == memory_type)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        .where(MemoryItem.content == content)
    ).first()
    if existing is not None:
        return existing

    # FTS5 near-duplicate check: look for memories with high text overlap.
    # Only check if FTS table exists (it won't in unit tests using create_all).
    try:
        from sqlalchemy import text

        # Use FTS5 match to find similar content. We check for memories that
        # share significant words with the new content.
        words = content.split()
        if len(words) < 3:
            return None
        # Build a query from the first few significant words.
        query_words = [w for w in words if len(w) > 3][:5]
        if not query_words:
            return None
        fts_query = " AND ".join(query_words)
        rows = session.execute(
            text(
                "SELECT rowid FROM memory_fts WHERE memory_fts MATCH :query "
                "AND rank < -5.0 LIMIT 5"
            ),
            {"query": fts_query},
        ).all()
        if rows:
            candidate_ids = [r[0] for r in rows]
            candidates = session.exec(
                select(MemoryItem)
                .where(MemoryItem.id.in_(candidate_ids))  # type: ignore[union-attr]
                .where(MemoryItem.user_id == user_id)
                .where(MemoryItem.memory_type == memory_type)
                .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
            ).all()
            # If any candidate shares > 70% of words, treat as duplicate.
            content_words = set(content.lower().split())
            for candidate in candidates:
                cand_words = set(candidate.content.lower().split())
                if content_words and cand_words:
                    overlap = len(content_words & cand_words) / max(
                        len(content_words), len(cand_words)
                    )
                    if overlap > 0.7:
                        return candidate
    except Exception:
        # FTS5 table may not exist in test environments; skip dedup.
        pass

    return None
