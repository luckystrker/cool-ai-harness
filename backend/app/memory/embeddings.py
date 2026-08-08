"""Embedding service for hybrid memory retrieval (Фаза 3a §1 hybrid index).

Wraps the sqlite-vec ``memory_vec`` virtual table (created by migration 0021)
plus the ``memory_embeddings`` metadata table. The vector lives in vec0; the
regular table tracks the producing model and dimension so the backfill sweep
can re-index.

Graceful degradation is the contract here:
- No ``LLMProvider.embed()`` support (or a failing call) → ``ensure_embedding``
  returns False and retrieval simply skips the vector leg (FTS5 still works).
- No vec0 table / extension (dev DBs bootstrapped with ``create_all``, or
  Python builds without loadable extensions) → ``vec_table_ready`` is False
  and every vector path no-ops.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime

from sqlalchemy import text
from sqlmodel import Session, col, select

from app.core.db import VEC_AVAILABLE, engine
from app.core.logging import get_logger
from app.memory.models import MemoryEmbedding

log = get_logger(__name__)

# vec0 virtual table name + metadata table name. Keep in sync with migration
# 0021 and the model definitions.
VEC_TABLE = "memory_vec"

# One of these models in the vec0 KNN query; produced by sqlite-vec.
_VEC_MATCH_SQL = (
    f"SELECT memory_id, distance FROM {VEC_TABLE} WHERE embedding MATCH :query AND k = :k"
)


def vec_table_ready() -> bool:
    """Whether the vec0 table exists and the extension is loaded.

    Checks the engine's DB for the virtual table (cheap sqlite_master lookup,
    re-checked every call so tests creating the table after startup work).
    When False, all vector operations no-op (FTS5-only retrieval).
    """
    if not VEC_AVAILABLE:
        return False
    try:
        with engine.connect() as conn:
            row = conn.execute(
                text("SELECT 1 FROM sqlite_master WHERE type='table' AND name=:name"),
                {"name": VEC_TABLE},
            ).first()
        return row is not None
    except Exception:
        return False


def upsert_vector(session: Session, *, memory_id: int, embedding: list[float], model: str) -> bool:
    """Insert or replace a memory's vector in the vec0 table + metadata row.

    vec0 has no UPDATE semantics: replace = delete + insert. The metadata row
    is upserted on ``(memory_id)``.
    """
    if not VEC_AVAILABLE or memory_id is None:
        return False
    vector_json = json.dumps(embedding, ensure_ascii=False)
    try:
        session.execute(text(f"DELETE FROM {VEC_TABLE} WHERE memory_id = :id"), {"id": memory_id})
        session.execute(
            text(f"INSERT INTO {VEC_TABLE}(memory_id, embedding) VALUES (:id, :emb)"),
            {"id": memory_id, "emb": vector_json},
        )
        existing = session.exec(
            select(MemoryEmbedding).where(MemoryEmbedding.memory_id == memory_id)
        ).first()
        now = datetime.now(UTC)
        if existing is not None:
            existing.model = model
            existing.dimension = len(embedding)
            existing.updated_at = now
            session.add(existing)
        else:
            session.add(
                MemoryEmbedding(
                    memory_id=memory_id,
                    model=model,
                    dimension=len(embedding),
                )
            )
        session.commit()
        return True
    except Exception as exc:
        log.warning("memory.vector_upsert_failed", memory_id=memory_id, error=str(exc))
        session.rollback()
        return False


def delete_vector(session: Session, *, memory_id: int) -> None:
    """Remove a memory's vector and metadata row (hard-delete cleanup).

    Commits its own transaction. Failures (e.g. missing vec0 table) are logged
    and swallowed — callers must not have their own writes rolled back.
    """
    if not VEC_AVAILABLE:
        return
    try:
        session.execute(text(f"DELETE FROM {VEC_TABLE} WHERE memory_id = :id"), {"id": memory_id})
        # Metadata row: delete by direct SQL for simplicity.
        session.execute(
            text("DELETE FROM memory_embeddings WHERE memory_id = :id"), {"id": memory_id}
        )
        session.commit()
    except Exception as exc:
        log.warning("memory.vector_delete_failed", memory_id=memory_id, error=str(exc))
        session.rollback()
        raise


def search_vectors(
    session: Session,
    *,
    embedding: list[float],
    limit: int = 20,
) -> list[tuple[int, float]]:
    """KNN search over the vec0 table. Returns [(memory_id, distance), ...].

    Distances are cosine distances (the vec0 table uses
    ``distance_metric=cosine``). Empty list when vectors are unavailable
    (missing extension, missing virtual table, malformed query).
    """
    if not VEC_AVAILABLE:
        return []
    try:
        rows = session.execute(
            text(_VEC_MATCH_SQL),
            {"query": json.dumps(embedding, ensure_ascii=False), "k": limit},
        ).all()
        return [(int(r[0]), float(r[1])) for r in rows]
    except Exception as exc:
        log.debug("memory.vector_search_unavailable", error=str(exc))
        return []


async def ensure_embedding(
    session: Session,
    *,
    provider,
    model: str | None,
    memory_id: int,
    content: str,
    dimension: int,
) -> bool:
    """Compute and store the embedding for a memory. Returns True on success.

    Best-effort: any failure (unsupported embed, network, dimension mismatch)
    leaves the memory without a vector — FTS5 retrieval covers it, and the
    daily backfill sweep retries later.
    """
    if not vec_table_ready() or not content.strip():
        return False
    if provider is None:
        return False
    try:
        vectors = await provider.embed([content], model=model)
    except Exception as exc:
        log.warning("memory.embed_compute_failed", memory_id=memory_id, error=str(exc))
        return False
    if not vectors:
        return False
    embedding = vectors[0]
    if len(embedding) != dimension:
        log.warning(
            "memory.embed_dim_mismatch",
            memory_id=memory_id,
            expected=dimension,
            actual=len(embedding),
        )
        return False
    return upsert_vector(
        session, memory_id=memory_id, embedding=embedding, model=model or "default"
    )


def missing_embedding_memory_ids(
    session: Session, *, user_id: int | None = None, limit: int = 100
) -> list[int]:
    """Active memories that have no vector yet (backfill targets)."""
    from app.memory.models import MEMORY_STATUS_ACTIVE, MemoryItem

    sub = select(MemoryEmbedding.memory_id)
    stmt = (
        select(MemoryItem.id)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        .where(col(MemoryItem.id).not_in(sub))
    )
    if user_id is not None:
        stmt = stmt.where(MemoryItem.user_id == user_id)
    rows = session.exec(stmt.limit(limit)).all()
    return [r for r in rows if r is not None]


async def backfill_embeddings(
    session: Session,
    *,
    provider,
    model: str | None,
    dimension: int,
    user_id: int | None = None,
    limit: int = 50,
) -> int:
    """Embed active memories that lack a vector (bounded per call)."""
    if not vec_table_ready() or provider is None:
        return 0
    from app.memory.models import MemoryItem

    ids = missing_embedding_memory_ids(session, user_id=user_id, limit=limit)
    done = 0
    for memory_id in ids:
        item = session.get(MemoryItem, memory_id)
        if item is None:
            continue
        ok = await ensure_embedding(
            session,
            provider=provider,
            model=model,
            memory_id=memory_id,
            content=item.content,
            dimension=dimension,
        )
        if ok:
            done += 1
    if done:
        log.info("memory.embedding_backfill", embedded=done, limit=limit)
    return done
