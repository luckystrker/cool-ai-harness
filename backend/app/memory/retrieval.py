"""Memory retrieval pipeline — FTS5 search + reranking + scope visibility.

Implements the VectorIndex protocol for future migration to embeddings/Qdrant.
Currently uses SQLite FTS5 (BM25) for full-text search with a composite
reranking score: fts_rank + importance + recency + confidence.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Protocol, runtime_checkable

from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.memory.models import (
    MEMORY_STATUS_ACTIVE,
    MEMORY_TYPE_PREFERENCE,
    SCOPE_AGENT,
    SCOPE_CONVERSATION,
    SCOPE_GLOBAL,
    MemoryItem,
)

log = get_logger(__name__)


# --- Vector Index Protocol (future migration path) ---


@runtime_checkable
class VectorIndex(Protocol):
    """Interface for vector-based memory search.

    Phase 1: FTS5Index (keyword search, no embeddings).
    Phase 2: HybridIndex (FTS5 + embeddings via LLMProvider.embed()).
    Phase 3: QdrantIndex (full vector DB, docker-compose service).
    """

    def upsert(self, memory_id: int, embedding: list[float]) -> None:
        """Index or re-index a memory's embedding."""
        ...

    def search(
        self, query_embedding: list[float], limit: int, filters: dict
    ) -> list[tuple[int, float]]:
        """Search for similar memories. Returns (memory_id, similarity) pairs."""
        ...


# --- Retrieval ---


def retrieve_memories(
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
    """Retrieve memories visible to the given context, ranked by relevance.

    Only ``active`` memories are returned. ``pending_confirmation`` memories
    (agent-extracted, not yet user-reviewed) are excluded by every fetch path —
    they never reach the agent's context until the user confirms them.

    Pipeline:
    1. Always include active preferences (high priority, always in context).
    2. If query is provided, run FTS5 full-text search.
    3. Filter by scope visibility rules.
    4. Rerank by composite score: relevance + importance + recency + confidence.
    5. Touch access metadata on returned memories.
    """
    results: list[MemoryItem] = []
    seen_ids: set[int] = set()

    # Resolve the current conversation's working directory (project key) so
    # conversation-scoped memories from the same project are visible.
    project_key = _resolve_project_key(session, conversation_id)

    # 1. Always fetch preferences (they're always relevant).
    preferences = _fetch_preferences(session, user_id=user_id)
    for pref in preferences:
        if pref.id is not None:
            results.append(pref)
            seen_ids.add(pref.id)

    # 2. FTS5 search (if query provided and FTS table exists).
    if query:
        fts_results = _fts5_search(
            session,
            query=query,
            user_id=user_id,
            agent_id=agent_id,
            conversation_id=conversation_id,
            project_key=project_key,
            memory_type=memory_type,
            limit=limit * 3,  # over-fetch for reranking
        )
        for item in fts_results:
            if item.id is not None and item.id not in seen_ids:
                results.append(item)
                seen_ids.add(item.id)

    # 3. If no query or FTS returned few results, fall back to recent important memories.
    if len(results) < limit:
        fallback = _fetch_recent_important(
            session,
            user_id=user_id,
            agent_id=agent_id,
            conversation_id=conversation_id,
            project_key=project_key,
            memory_type=memory_type,
            limit=limit * 2,
        )
        for item in fallback:
            if item.id is not None and item.id not in seen_ids:
                results.append(item)
                seen_ids.add(item.id)

    # 4. Rerank by composite score.
    now = datetime.now(UTC)
    scored = [(item, _score_memory(item, now)) for item in results]
    scored.sort(key=lambda x: x[1], reverse=True)

    # 5. Touch access metadata and return top-limit.
    final = [item for item, _ in scored[:limit]]
    _touch_access(session, final, now)
    return final


def _fetch_preferences(session: Session, *, user_id: int) -> list[MemoryItem]:
    """Fetch active preference memories (always injected into context)."""
    return list(
        session.exec(
            select(MemoryItem)
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.memory_type == MEMORY_TYPE_PREFERENCE)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
            .order_by(col(MemoryItem.importance).desc())
            .limit(10)
        ).all()
    )


def _fts5_search(
    session: Session,
    *,
    query: str,
    user_id: int,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    project_key: str | None = None,
    memory_type: str | None = None,
    limit: int = 30,
) -> list[MemoryItem]:
    """Run FTS5 full-text search and return matching MemoryItems.

    Falls back gracefully if the FTS5 table doesn't exist (test environments).
    """
    try:
        from sqlalchemy import text

        # Tokenize the query for FTS5: use OR between words for broader matching.
        words = [w.strip() for w in query.split() if len(w.strip()) > 1]
        if not words:
            return []
        # Limit query terms to avoid overly broad searches.
        fts_query = " OR ".join(words[:10])

        rows = session.execute(
            text(
                "SELECT rowid, rank FROM memory_fts "
                "WHERE memory_fts MATCH :query "
                "ORDER BY rank LIMIT :limit"
            ),
            {"query": fts_query, "limit": limit},
        ).all()

        if not rows:
            return []

        # Fetch the actual memory items and apply scope filters.
        candidate_ids = [r[0] for r in rows]
        rank_map = {r[0]: r[1] for r in rows}

        candidates = session.exec(
            select(MemoryItem)
            .where(MemoryItem.id.in_(candidate_ids))  # type: ignore[union-attr]
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        ).all()

        # Apply scope visibility filter.
        visible = [
            item
            for item in candidates
            if _is_visible(
                item,
                agent_id=agent_id,
                conversation_id=conversation_id,
                project_key=project_key,
            )
        ]

        # Apply memory_type filter if specified.
        if memory_type:
            visible = [item for item in visible if item.memory_type == memory_type]

        # Sort by FTS rank (lower = better in BM25).
        visible.sort(key=lambda item: rank_map.get(item.id, 0))
        return visible

    except Exception as exc:
        # FTS5 table may not exist in test environments using create_all().
        log.debug("memory.fts5_unavailable", error=str(exc))
        return []


def _fetch_recent_important(
    session: Session,
    *,
    user_id: int,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    project_key: str | None = None,
    memory_type: str | None = None,
    limit: int = 20,
) -> list[MemoryItem]:
    """Fetch recent, important memories as a fallback when FTS5 isn't available."""
    stmt = (
        select(MemoryItem)
        .where(MemoryItem.user_id == user_id)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
    )
    if memory_type:
        stmt = stmt.where(MemoryItem.memory_type == memory_type)

    # Scope filter: global + agent-specific + conversation/project-specific.
    scope_conditions = [MemoryItem.scope == SCOPE_GLOBAL]
    if agent_id is not None:
        scope_conditions.append(
            (MemoryItem.scope == SCOPE_AGENT) & (MemoryItem.agent_id == agent_id)
        )
    if conversation_id is not None:
        scope_conditions.append(
            (MemoryItem.scope == SCOPE_CONVERSATION)
            & (MemoryItem.conversation_id == conversation_id)
        )
    # Project-level visibility: conversation-scoped memories whose _project_key
    # (working directory) matches the current conversation's project.
    if project_key is not None:
        from sqlalchemy import func

        scope_conditions.append(
            (MemoryItem.scope == SCOPE_CONVERSATION)  # type: ignore[operator]
            & (func.json_extract(MemoryItem.structured, "$._project_key") == project_key)
        )

    from sqlalchemy import or_

    stmt = stmt.where(or_(False, *scope_conditions))  # type: ignore[arg-type]
    stmt = stmt.order_by(col(MemoryItem.importance).desc(), col(MemoryItem.updated_at).desc())
    stmt = stmt.limit(limit)

    return list(session.exec(stmt).all())


def _resolve_project_key(session: Session, conversation_id: int | None) -> str | None:
    """Resolve the working directory (project key) for a conversation."""
    if conversation_id is None:
        return None
    from app.models import Conversation

    conv = session.get(Conversation, conversation_id)
    return conv.working_directory if conv else None


def _is_visible(
    item: MemoryItem,
    *,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    project_key: str | None = None,
) -> bool:
    """Check if a memory is visible given the current agent/conversation context.

    Conversation-scoped memories are visible when:
    - the conversation_id matches, OR
    - the memory's _project_key (working directory) matches the current
      conversation's project (project-level visibility).
    """
    if item.scope == SCOPE_GLOBAL:
        return True
    if item.scope == SCOPE_AGENT:
        return agent_id is not None and item.agent_id == agent_id
    if item.scope == SCOPE_CONVERSATION:
        if conversation_id is not None and item.conversation_id == conversation_id:
            return True
        # Project-level visibility via _project_key.
        if project_key is not None:
            mem_project_key = (item.structured or {}).get("_project_key")
            if mem_project_key == project_key:
                return True
        return False
    return False


# Weighting for the composite relevance score. Exposed here so the breakdown
# returned to callers ("why remembered") uses the same constants as ranking.
_W_IMPORTANCE = 0.25
_W_RECENCY = 0.25
_W_CONFIDENCE = 0.15
_W_TYPE = 0.35

# type_priority lookup shared by ranking and explanation.
TYPE_PRIORITY = {
    "preference": 1.0,
    "procedural": 0.8,
    "semantic": 0.6,
    "episodic": 0.4,
}


def score_memory(item: MemoryItem, now: datetime | None = None) -> dict[str, float]:
    """Compute the composite relevance score AND its component breakdown.

    Used both for reranking (``total``) and for the "why is this remembered"
    explanation surfaced to the agent/UI. Returns a dict::

        {
            "total": float,            # weighted composite
            "importance": float,       # contribution from importance
            "recency": float,          # contribution from recency
            "confidence": float,       # contribution from confidence
            "type_priority": float,    # contribution from memory type
            "age_days": float,         # how stale the memory is
        }
    """
    if now is None:
        now = datetime.now(UTC)

    # Recency: exponential decay over 30 days.
    # Handle naive datetimes from SQLite (strip tzinfo for comparison).
    updated = item.updated_at
    if updated is not None:
        # Make both naive for comparison (SQLite stores naive datetimes).
        now_naive = now.replace(tzinfo=None) if now.tzinfo else now
        updated_naive = updated.replace(tzinfo=None) if updated.tzinfo else updated
        age_days = (now_naive - updated_naive).total_seconds() / 86400
    else:
        age_days = 30.0
    recency = 1.0 / (1.0 + age_days / 30.0)

    type_priority = TYPE_PRIORITY.get(item.memory_type, 0.5)

    return {
        "importance": _W_IMPORTANCE * item.importance,
        "recency": _W_RECENCY * recency,
        "confidence": _W_CONFIDENCE * item.confidence,
        "type_priority": _W_TYPE * type_priority,
        "age_days": age_days,
        "total": (
            _W_IMPORTANCE * item.importance
            + _W_RECENCY * recency
            + _W_CONFIDENCE * item.confidence
            + _W_TYPE * type_priority
        ),
    }


def _score_memory(item: MemoryItem, now: datetime) -> float:
    """Composite relevance score for reranking (scalar, for sort key)."""
    return score_memory(item, now)["total"]


def _touch_access(session: Session, items: list[MemoryItem], now: datetime) -> None:
    """Update access metadata for retrieved memories."""
    for item in items:
        item.access_count += 1
        item.last_accessed_at = now
        session.add(item)
    if items:
        session.commit()
