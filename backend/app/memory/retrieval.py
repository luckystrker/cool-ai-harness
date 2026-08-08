"""Memory retrieval pipeline — FTS5 + vector search + reranking + scope visibility.

Implements the VectorIndex protocol for future migration to embeddings/Qdrant.
Currently uses SQLite FTS5 (BM25) full-text search plus an optional sqlite-vec
vector leg (hybrid retrieval), with a composite reranking score: relevance +
importance + recency + confidence + type priority.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime
from typing import Protocol, runtime_checkable

from sqlmodel import Session, col, select

from app.core.config import get_settings
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


# --- FTS5 query helpers ---


def _fts_quote(term: str) -> str:
    """Quote a single term as an FTS5 phrase, escaping embedded quotes.

    Quoting makes every operator character literal: no NEAR/OR/AND/column
    syntax, no ``*``/``^``/``-`` surprises, no query-language injection.
    """
    return '"' + term.replace('"', '""') + '"'


def _fts_terms(query: str) -> list[str]:
    """Tokenize a user query into safe FTS5 phrase terms.

    Keeps Unicode letters/digits/underscore (so Cyrillic and other alphabets
    survive — the unicode61 tokenizer handles them natively) and drops
    everything else.
    """
    words = [re.sub(r"[^\w]", "", w.strip(), flags=re.UNICODE) for w in query.split()]
    return [w for w in words if len(w) > 1]


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
    include_preferences: bool = True,
    query_embedding: list[float] | None = None,
) -> list[MemoryItem]:
    """Retrieve memories visible to the given context, ranked by relevance.

    Only ``active`` memories are returned. ``pending_confirmation`` memories
    (agent-extracted, not yet user-reviewed) are excluded by every fetch path —
    they never reach the agent's context until the user confirms them.

    Pipeline:
    1. Optionally include active preferences (high priority, always in context).
    2. If query is provided, run FTS5 full-text search.
    3. If query is provided and hybrid retrieval is enabled, run a vector KNN
       search and merge the results (a memory matched by either signal gets a
       relevance boost).
    4. Filter by scope visibility rules.
    5. Rerank by composite score: relevance + importance + recency + confidence
       + type priority.
    6. Touch access metadata on returned memories.

    ``query_embedding`` is the pre-computed embedding of ``query`` (callers
    that own an LLMProvider may embed the query before calling; when None the
    vector leg is skipped).
    """
    results: list[MemoryItem] = []
    seen_ids: set[int] = set()

    # Resolve the current conversation's working directory (project key) so
    # conversation-scoped memories from the same project are visible.
    project_key = _resolve_project_key(session, conversation_id)

    # 1. Preferences (they're always relevant) — skip when the caller wants a
    # pure non-preference block (context builder already has its own section).
    if include_preferences:
        preferences = _fetch_preferences(session, user_id=user_id)
        for pref in preferences:
            if pref.id is not None:
                results.append(pref)
                seen_ids.add(pref.id)

    # 2.+3. FTS5 + vector search (if query provided).
    relevance: dict[int, float] = {}
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
        for item, rel in fts_results:
            if item.id is not None:
                relevance[item.id] = max(relevance.get(item.id, 0.0), rel)
                if item.id not in seen_ids:
                    results.append(item)
                    seen_ids.add(item.id)

        if query_embedding:
            vec_results = _vec_search(
                session,
                embedding=query_embedding,
                user_id=user_id,
                agent_id=agent_id,
                conversation_id=conversation_id,
                project_key=project_key,
                memory_type=memory_type,
                limit=limit * 3,
            )
            for item, rel in vec_results:
                if item.id is not None:
                    relevance[item.id] = max(relevance.get(item.id, 0.0), rel)
                    if item.id not in seen_ids:
                        results.append(item)
                        seen_ids.add(item.id)

        # Entity-linked memories: when the query names an entity, pull the
        # memories linked to it (C2 — entity-driven recall).
        entity_results = _entity_search(
            session,
            query=query,
            user_id=user_id,
            agent_id=agent_id,
            conversation_id=conversation_id,
            project_key=project_key,
            memory_type=memory_type,
            limit=3,
        )
        for item in entity_results:
            if item.id is not None:
                relevance.setdefault(item.id, 0.35)
                if item.id not in seen_ids:
                    results.append(item)
                    seen_ids.add(item.id)

    # 4. If no query or FTS returned few results, fall back to recent important memories.
    if len(results) < limit:
        fallback = _fetch_recent_important(
            session,
            user_id=user_id,
            agent_id=agent_id,
            conversation_id=conversation_id,
            project_key=project_key,
            memory_type=memory_type,
            limit=limit * 2,
            exclude_preferences=not include_preferences,
        )
        for item in fallback:
            if item.id is not None and item.id not in seen_ids:
                results.append(item)
                seen_ids.add(item.id)

    # 5. Rerank by composite score (relevance-aware when a query was given).
    now = datetime.now(UTC)
    scored = [
        (
            item,
            _score_memory(item, now, relevance=relevance.get(item.id))
            if item.id is not None
            else 0.0,
        )
        for item in results
    ]
    scored.sort(key=lambda x: x[1], reverse=True)

    # 6. Touch access metadata and return top-limit.
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
) -> list[tuple[MemoryItem, float]]:
    """Run FTS5 full-text search and return (item, relevance) pairs.

    Falls back gracefully if the FTS5 table doesn't exist (test environments).
    """
    try:
        from sqlalchemy import text

        # Tokenize the query for FTS5: quoted terms joined with OR — broad
        # matching, no query-language injection (B1).
        terms = _fts_terms(query)
        if not terms:
            return []
        # Limit query terms to avoid overly broad searches.
        fts_query = " OR ".join(_fts_quote(t) for t in terms[:16])

        settings = get_settings()
        min_rank = settings.memory_fts_min_rank
        rows = session.execute(
            text(
                "SELECT rowid, rank FROM memory_fts "
                "WHERE memory_fts MATCH :query AND rank < :min_rank "
                "ORDER BY rank LIMIT :limit"
            ),
            {"query": fts_query, "min_rank": min_rank, "limit": limit},
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

        # Map each item to a normalized relevance from its BM25 rank (lower =
        # better). ``rel`` shrinks as the rank gets worse; ranks at or beyond
        # the threshold map to ~0.
        out: list[tuple[MemoryItem, float]] = []
        for item in visible:
            rank = rank_map.get(item.id, 0.0)
            rel = 1.0 / (1.0 + abs(rank) / 3.0)
            out.append((item, rel))
        out.sort(key=lambda x: rank_map.get(x[0].id, 0))
        return out

    except Exception as exc:
        # FTS5 table may not exist in test environments using create_all().
        log.debug("memory.fts5_unavailable", error=str(exc))
        return []


def _vec_search(
    session: Session,
    *,
    embedding: list[float],
    user_id: int,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    project_key: str | None = None,
    memory_type: str | None = None,
    limit: int = 30,
) -> list[tuple[MemoryItem, float]]:
    """Vector KNN leg of hybrid retrieval. Returns (item, relevance) pairs.

    Relevance = 1 - cosine_distance (clipped at 0). No-ops (empty list) when
    embeddings are unavailable.
    """
    settings = get_settings()
    if not settings.memory_hybrid_enabled:
        return []
    try:
        from app.memory import embeddings as emb

        vec_hits = emb.search_vectors(session, embedding=embedding, limit=limit * 3)
        if not vec_hits:
            return []
        ids = [mid for mid, _ in vec_hits]
        dist_map = {mid: d for mid, d in vec_hits}
        candidates = session.exec(
            select(MemoryItem)
            .where(MemoryItem.id.in_(ids))  # type: ignore[union-attr]
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        ).all()
        out: list[tuple[MemoryItem, float]] = []
        for item in candidates:
            if item.id is None:
                continue
            if not _is_visible(
                item,
                agent_id=agent_id,
                conversation_id=conversation_id,
                project_key=project_key,
            ):
                continue
            if memory_type and item.memory_type != memory_type:
                continue
            rel = max(0.0, 1.0 - dist_map.get(item.id, 2.0))
            out.append((item, rel))
        out.sort(key=lambda x: dist_map.get(x[0].id or 0, 2.0))
        return out
    except Exception as exc:
        log.debug("memory.vec_search_unavailable", error=str(exc))
        return []


def _entity_search(
    session: Session,
    *,
    query: str,
    user_id: int,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    project_key: str | None = None,
    memory_type: str | None = None,
    limit: int = 3,
) -> list[MemoryItem]:
    """Pull memories linked to entities named in the query (C2)."""
    try:
        from app.memory.entities import batch_match_entities, memories_for_entity

        words = [
            w.strip(".,;:!?()[]\"'") for w in query.split() if len(w.strip(".,;:!?()[]\"'")) > 3
        ]
        if not words:
            return []
        entities = batch_match_entities(session, user_id=user_id, words=words[:5], limit=6)
        if not entities:
            return []
        seen: set[int] = set()
        out: list[MemoryItem] = []
        for ent in entities:
            if ent.id is None:
                continue
            for mem in memories_for_entity(session, entity_id=ent.id, active_only=True):
                if mem.id is None or mem.id in seen:
                    continue
                if mem.user_id != user_id:
                    continue
                if not _is_visible(
                    mem,
                    agent_id=agent_id,
                    conversation_id=conversation_id,
                    project_key=project_key,
                ):
                    continue
                if memory_type and mem.memory_type != memory_type:
                    continue
                seen.add(mem.id)
                out.append(mem)
                if len(out) >= limit:
                    return out
        return out
    except Exception as exc:
        log.debug("memory.entity_search_unavailable", error=str(exc))
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
    exclude_preferences: bool = False,
) -> list[MemoryItem]:
    """Fetch recent, important memories as a fallback when FTS5 isn't available."""
    stmt = (
        select(MemoryItem)
        .where(MemoryItem.user_id == user_id)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
    )
    if memory_type:
        stmt = stmt.where(MemoryItem.memory_type == memory_type)
    if exclude_preferences:
        stmt = stmt.where(MemoryItem.memory_type != MEMORY_TYPE_PREFERENCE)

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

# Weights used when a query provided a relevance signal (hybrid/FTS hit).
# Relevance dominates so an exact match outranks a stale high-importance fact.
_W_REL = 0.40
_W_REL_IMPORTANCE = 0.20
_W_REL_RECENCY = 0.15
_W_REL_CONFIDENCE = 0.10
_W_REL_TYPE = 0.15

# type_priority lookup shared by ranking and explanation.
TYPE_PRIORITY = {
    "preference": 1.0,
    "procedural": 0.8,
    "semantic": 0.6,
    "episodic": 0.4,
}


def score_memory(
    item: MemoryItem, now: datetime | None = None, relevance: float | None = None
) -> dict[str, float]:
    """Compute the composite relevance score AND its component breakdown.

    Used both for reranking (``total``) and for the "why is this remembered"
    explanation surfaced to the agent/UI. When ``relevance`` is provided (a
    query matched this memory via FTS/vectors), the score shifts weight from
    generic components to relevance. Returns a dict::

        {
            "total": float,            # weighted composite
            "importance": float,       # contribution from importance
            "recency": float,          # contribution from recency
            "confidence": float,       # contribution from confidence
            "type_priority": float,    # contribution from memory type
            "relevance": float,        # contribution from query relevance (0 if none)
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

    if relevance is not None:
        total = (
            _W_REL * relevance
            + _W_REL_IMPORTANCE * item.importance
            + _W_REL_RECENCY * recency
            + _W_REL_CONFIDENCE * item.confidence
            + _W_REL_TYPE * type_priority
        )
        return {
            "importance": _W_REL_IMPORTANCE * item.importance,
            "recency": _W_REL_RECENCY * recency,
            "confidence": _W_REL_CONFIDENCE * item.confidence,
            "type_priority": _W_REL_TYPE * type_priority,
            "relevance": _W_REL * relevance,
            "age_days": age_days,
            "total": total,
        }

    return {
        "importance": _W_IMPORTANCE * item.importance,
        "recency": _W_RECENCY * recency,
        "confidence": _W_CONFIDENCE * item.confidence,
        "type_priority": _W_TYPE * type_priority,
        "relevance": 0.0,
        "age_days": age_days,
        "total": (
            _W_IMPORTANCE * item.importance
            + _W_RECENCY * recency
            + _W_CONFIDENCE * item.confidence
            + _W_TYPE * type_priority
        ),
    }


def _score_memory(item: MemoryItem, now: datetime, relevance: float | None = None) -> float:
    """Composite relevance score for reranking (scalar, for sort key)."""
    return score_memory(item, now, relevance=relevance)["total"]


def _touch_access(session: Session, items: list[MemoryItem], now: datetime) -> None:
    """Update access metadata for retrieved memories."""
    for item in items:
        item.access_count += 1
        item.last_accessed_at = now
        session.add(item)
    if items:
        session.commit()
