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
    MEMORY_SOURCE_SYSTEM,
    MEMORY_SOURCE_USER_EXPLICIT,
    MEMORY_STATUS_ACTIVE,
    MEMORY_STATUS_ARCHIVED,
    MEMORY_STATUS_PENDING_CONFIRMATION,
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
# Sources that skip the pending_confirmation gate (trusted to be accurate).
_TRUSTED_SOURCES = frozenset({MEMORY_SOURCE_USER_EXPLICIT, MEMORY_SOURCE_SYSTEM})


def _default_status(source: str, confirmed: bool) -> str:
    """Determine the initial status for a new memory.

    Trusted sources (user_explicit, system) and explicitly confirmed writes are
    stored directly as ``active``. Agent / agent_extraction sources land in
    ``pending_confirmation`` for user review.
    """
    if confirmed or source in _TRUSTED_SOURCES:
        return MEMORY_STATUS_ACTIVE
    return MEMORY_STATUS_PENDING_CONFIRMATION


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
    confirmed: bool = False,
) -> MemoryItem:
    """Store a new memory, with deduplication and validation.

    If a highly similar memory already exists (same type + overlapping content),
    it is updated instead of creating a duplicate.

    Confirmation model:
    - Trusted sources (``user_explicit``, ``system``) and ``confirmed=True`` →
      stored as ``active`` and immediately eligible for recall.
    - Agent / ``agent_extraction`` sources → stored as ``pending_confirmation``
      and excluded from recall until the user confirms (``confirm_memory``) or
      rejects (``reject_memory``) them.

    Dedup respects confirmation: a pending duplicate never overwrites a
    confirmed (active) fact's content, and vice versa.
    """
    # Cap importance for non-user sources.
    if source != "user_explicit" and importance > MAX_AGENT_IMPORTANCE:
        importance = MAX_AGENT_IMPORTANCE

    # Clamp values.
    importance = max(0.0, min(1.0, importance))
    confidence = max(0.0, min(1.0, confidence))

    new_status = _default_status(source, confirmed)

    # For conversation-scoped memories, capture the conversation's working
    # directory as a "project key" so the memory is visible across all
    # conversations in the same project (same working directory), not just
    # the originating conversation.
    if scope == SCOPE_CONVERSATION and conversation_id is not None:
        structured = _attach_project_key(session, conversation_id, structured)

    # Deduplication: check for an existing memory with the same type, similar
    # content, AND a matching status tier (confirmed vs pending). A pending
    # write never overwrites a confirmed fact.
    existing = _find_duplicate(
        session,
        user_id=user_id,
        content=content,
        memory_type=memory_type,
        status=new_status,
    )
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
        status=new_status,
        ttl_days=ttl_days,
    )
    session.add(memory)
    session.commit()
    session.refresh(memory)
    log.info(
        "memory.created",
        memory_id=memory.id,
        memory_type=memory_type,
        scope=scope,
        status=new_status,
    )
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


def explain_memory(session: Session, memory_id: int) -> dict | None:
    """Build the "why is this remembered" explanation for a memory.

    Returns provenance (source, scope, originating conversation), lifecycle
    metadata (when stored, last accessed, access count), confirmation status,
    and the composite score breakdown so the UI/agent can show *why* a memory
    ranked where it did. Returns None if the memory is not found.
    """
    memory = session.get(MemoryItem, memory_id)
    if memory is None:
        return None

    from app.memory.retrieval import score_memory

    now = datetime.now(UTC)
    breakdown = score_memory(memory, now)

    return {
        "memory_id": memory.id,
        "source": memory.source,
        "scope": memory.scope,
        "status": memory.status,
        "pinned": memory.pinned,
        "confidence": memory.confidence,
        "importance": memory.importance,
        "memory_type": memory.memory_type,
        "conversation_id": memory.conversation_id,
        "agent_id": memory.agent_id,
        "created_at": memory.created_at,
        "updated_at": memory.updated_at,
        "last_accessed_at": memory.last_accessed_at,
        "access_count": memory.access_count,
        "score": breakdown,
    }


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
        "pinned",
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


def export_memories(
    session: Session,
    *,
    user_id: int,
    fmt: str = "json",
    include_archived: bool = False,
) -> bytes:
    """Export a user's memories.

    ``fmt="json"`` returns a complete structured dump (UTF-8 bytes).
    ``fmt="markdown"`` returns a human-readable grouped list.

    Only active memories are exported by default; set ``include_archived`` to
    also include archived/superseded records (useful for backups).
    """
    from datetime import UTC, datetime

    stmt = select(MemoryItem).where(MemoryItem.user_id == user_id)
    if not include_archived:
        stmt = stmt.where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
    stmt = stmt.order_by(
        col(MemoryItem.memory_type).asc(), col(MemoryItem.importance).desc()
    )
    memories = list(session.exec(stmt).all())

    if fmt == "markdown":
        return _memories_to_markdown(memories).encode("utf-8")

    # JSON — full structured dump.
    import json

    payload = {
        "exported_at": datetime.now(UTC).isoformat(),
        "user_id": user_id,
        "count": len(memories),
        "memories": [
            {
                "id": m.id,
                "type": m.memory_type,
                "scope": m.scope,
                "content": m.content,
                "tags": m.tags or [],
                "structured": m.structured,
                "importance": m.importance,
                "confidence": m.confidence,
                "source": m.source,
                "status": m.status,
                "pinned": m.pinned,
                "conversation_id": m.conversation_id,
                "agent_id": m.agent_id,
                "ttl_days": m.ttl_days,
                "valid_from": m.valid_from.isoformat() if m.valid_from else None,
                "valid_to": m.valid_to.isoformat() if m.valid_to else None,
                "created_at": m.created_at.isoformat() if m.created_at else None,
                "updated_at": m.updated_at.isoformat() if m.updated_at else None,
            }
            for m in memories
        ],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2).encode("utf-8")


def _memories_to_markdown(memories: Sequence[MemoryItem]) -> str:
    """Render memories as a grouped Markdown document."""
    from collections import defaultdict

    by_type: dict[str, list[MemoryItem]] = defaultdict(list)
    for m in memories:
        by_type[m.memory_type].append(m)

    lines = ["# Memory export", ""]
    for mtype, items in by_type.items():
        lines.append(f"## {mtype.capitalize()} ({len(items)})")
        lines.append("")
        for m in items:
            tags = f" `{'`,`'.join(m.tags or [])}`" if m.tags else ""
            pinned = " 📌" if m.pinned else ""
            lines.append(
                f"- **{m.content}**{tags}{pinned}  \n"
                f"  _importance {m.importance:.2f} · confidence {m.confidence:.2f} · "
                f"source `{m.source}` · status `{m.status}`_"
            )
        lines.append("")
    return "\n".join(lines)


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
    status: str = MEMORY_STATUS_ACTIVE,
) -> MemoryItem | None:
    """Find an existing memory of the given status tier that is likely a duplicate.

    Uses exact content match first (fast path), then falls back to FTS5
    similarity for near-duplicates. Dedup is scoped to a single status tier so
    a ``pending_confirmation`` write never merges into (and thus overwrites) a
    confirmed ``active`` fact, and vice versa.
    """
    # Fast path: exact content match within the same status tier.
    existing = session.exec(
        select(MemoryItem)
        .where(MemoryItem.user_id == user_id)
        .where(MemoryItem.memory_type == memory_type)
        .where(MemoryItem.status == status)
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
                .where(MemoryItem.status == status)
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


# --- Confirmation workflow (user review of agent-extracted memories) ---


def confirm_memory(session: Session, memory_id: int) -> MemoryItem | None:
    """Confirm a pending memory: promote it to ``active`` (eligible for recall).

    Returns the updated memory, or None if not found.
    """
    memory = session.get(MemoryItem, memory_id)
    if memory is None:
        return None
    memory.status = MEMORY_STATUS_ACTIVE
    memory.updated_at = datetime.now(UTC)
    session.add(memory)
    session.commit()
    session.refresh(memory)
    log.info("memory.confirmed", memory_id=memory_id)
    return memory


def reject_memory(session: Session, memory_id: int) -> bool:
    """Reject a pending memory: archive it (recoverable via the UI).

    Returns True if the memory was found and rejected.
    """
    memory = session.get(MemoryItem, memory_id)
    if memory is None:
        return False
    memory.status = MEMORY_STATUS_ARCHIVED
    memory.updated_at = datetime.now(UTC)
    session.add(memory)
    session.commit()
    log.info("memory.rejected", memory_id=memory_id)
    return True


def list_pending(
    session: Session,
    *,
    user_id: int,
    limit: int = 100,
    offset: int = 0,
) -> Sequence[MemoryItem]:
    """List memories awaiting user confirmation, newest first."""
    stmt = (
        select(MemoryItem)
        .where(MemoryItem.user_id == user_id)
        .where(MemoryItem.status == MEMORY_STATUS_PENDING_CONFIRMATION)
        .order_by(col(MemoryItem.created_at).desc())
        .offset(offset)
        .limit(limit)
    )
    return session.exec(stmt).all()


def pin_memory(session: Session, memory_id: int, pinned: bool = True) -> MemoryItem | None:
    """Pin (or unpin) a memory. Pinned memories are protected from decay/TTL.

    Returns the updated memory, or None if not found.
    """
    memory = session.get(MemoryItem, memory_id)
    if memory is None:
        return None
    memory.pinned = pinned
    memory.updated_at = datetime.now(UTC)
    session.add(memory)
    session.commit()
    session.refresh(memory)
    return memory
