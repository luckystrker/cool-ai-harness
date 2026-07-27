"""Memory lifecycle management — decay, consolidation, TTL enforcement.

Periodic maintenance operations that keep the memory store healthy:
- Decay: reduce importance of unused memories over time.
- Consolidation: merge similar memories to reduce noise.
- Forgetting: archive memories below the importance threshold.
- TTL: auto-archive memories past their expiry date.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from sqlmodel import Session, select

from app.core.config import get_settings
from app.core.logging import get_logger
from app.memory.models import (
    MEMORY_STATUS_ACTIVE,
    MEMORY_STATUS_ARCHIVED,
    MemoryItem,
)

log = get_logger(__name__)

# Memories below this effective importance are archived.
ARCHIVE_THRESHOLD = 0.1
# Decay half-life in days (importance halves after this many days without access).
DECAY_HALF_LIFE_DAYS = 30.0


def _days_between(now: datetime, other: datetime) -> float:
    """Calculate days between two datetimes, handling naive/aware mismatch."""
    now_naive = now.replace(tzinfo=None) if now.tzinfo else now
    other_naive = other.replace(tzinfo=None) if other.tzinfo else other
    return (now_naive - other_naive).total_seconds() / 86400


def run_decay_sweep(session: Session, *, user_id: int | None = None) -> int:
    """Reduce importance of memories not accessed recently.

    Formula: effective_importance = importance * (1 / (1 + days_since_access / half_life))
    Memories whose effective importance drops below ARCHIVE_THRESHOLD are archived.

    Returns the number of memories archived.
    """
    settings = get_settings()
    if not settings.memory_decay_enabled:
        return 0

    now = datetime.now(UTC)
    stmt = select(MemoryItem).where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
    if user_id is not None:
        stmt = stmt.where(MemoryItem.user_id == user_id)

    memories = session.exec(stmt).all()
    archived_count = 0

    for memory in memories:
        # Calculate days since last access (or creation if never accessed).
        reference_date = memory.last_accessed_at or memory.created_at
        days_since = _days_between(now, reference_date)

        # Apply decay formula.
        decay_factor = 1.0 / (1.0 + days_since / DECAY_HALF_LIFE_DAYS)
        effective_importance = memory.importance * decay_factor

        # Archive if below threshold (but never archive preferences or high-confidence items).
        if (
            effective_importance < ARCHIVE_THRESHOLD
            and memory.memory_type != "preference"
            and memory.confidence < 0.9
        ):
            memory.status = MEMORY_STATUS_ARCHIVED
            memory.updated_at = now
            session.add(memory)
            archived_count += 1

    if archived_count > 0:
        session.commit()
        log.info("memory.decay_sweep", archived_count=archived_count)

    return archived_count


def run_ttl_sweep(session: Session, *, user_id: int | None = None) -> int:
    """Archive memories that have exceeded their TTL.

    Returns the number of memories archived.
    """
    now = datetime.now(UTC)
    stmt = (
        select(MemoryItem)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        .where(MemoryItem.ttl_days.isnot(None))  # type: ignore[union-attr]
    )
    if user_id is not None:
        stmt = stmt.where(MemoryItem.user_id == user_id)

    memories = session.exec(stmt).all()
    archived_count = 0

    for memory in memories:
        if memory.ttl_days is None:
            continue
        # Check if TTL has expired.
        reference = memory.valid_from or memory.created_at
        expiry = reference + timedelta(days=memory.ttl_days)
        if _days_between(now, expiry) > 0:  # now > expiry (TTL expired)
            memory.status = MEMORY_STATUS_ARCHIVED
            memory.updated_at = now
            session.add(memory)
            archived_count += 1

    if archived_count > 0:
        session.commit()
        log.info("memory.ttl_sweep", archived_count=archived_count)

    return archived_count


def run_validity_sweep(session: Session, *, user_id: int | None = None) -> int:
    """Archive memories whose valid_to date has passed.

    Returns the number of memories archived.
    """
    now = datetime.now(UTC)
    stmt = (
        select(MemoryItem)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        .where(MemoryItem.valid_to.isnot(None))  # type: ignore[union-attr]
        .where(MemoryItem.valid_to < now)  # type: ignore[operator]
    )
    if user_id is not None:
        stmt = stmt.where(MemoryItem.user_id == user_id)

    memories = session.exec(stmt).all()
    archived_count = 0

    for memory in memories:
        memory.status = MEMORY_STATUS_ARCHIVED
        memory.updated_at = now
        session.add(memory)
        archived_count += 1

    if archived_count > 0:
        session.commit()
        log.info("memory.validity_sweep", archived_count=archived_count)

    return archived_count


def find_consolidation_candidates(
    session: Session,
    *,
    user_id: int,
    memory_type: str | None = None,
    threshold: int | None = None,
) -> list[list[MemoryItem]]:
    """Find groups of similar memories that could be consolidated.

    Returns groups of memories with the same type and overlapping tags/content
    that exceed the consolidation threshold.
    """
    settings = get_settings()
    min_group_size = threshold or settings.memory_consolidation_threshold

    stmt = (
        select(MemoryItem)
        .where(MemoryItem.user_id == user_id)
        .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
    )
    if memory_type:
        stmt = stmt.where(MemoryItem.memory_type == memory_type)

    memories = list(session.exec(stmt).all())

    # Group by memory_type + first tag (simple heuristic).
    groups: dict[str, list[MemoryItem]] = {}
    for mem in memories:
        # Key: type + first tag (or "untagged").
        tag_key = (mem.tags[0] if mem.tags else "untagged")
        group_key = f"{mem.memory_type}:{tag_key}"
        groups.setdefault(group_key, []).append(mem)

    # Also group by content similarity (simple word overlap).
    # For now, just return groups that exceed the threshold.
    candidates = [group for group in groups.values() if len(group) >= min_group_size]
    return candidates


def consolidate_group(
    session: Session,
    group: list[MemoryItem],
    merged_content: str,
    merged_importance: float | None = None,
) -> MemoryItem:
    """Consolidate a group of memories into one.

    Creates a new memory with the merged content and marks the originals
    as superseded.
    """
    from app.memory.service import remember

    if not group:
        raise ValueError("Cannot consolidate empty group")

    # Use the first memory's metadata as the base.
    base = group[0]
    importance = merged_importance or max(m.importance for m in group)

    # Create the consolidated memory.
    consolidated = remember(
        session,
        user_id=base.user_id,
        content=merged_content,
        memory_type=base.memory_type,
        scope=base.scope,
        agent_id=base.agent_id,
        importance=importance,
        confidence=max(m.confidence for m in group),
        source="system",
        tags=base.tags,
    )

    # Mark originals as superseded.
    now = datetime.now(UTC)
    for mem in group:
        if mem.id != consolidated.id:
            mem.status = "superseded"
            mem.supersedes_id = consolidated.id
            mem.updated_at = now
            session.add(mem)

    session.commit()
    log.info(
        "memory.consolidated",
        consolidated_id=consolidated.id,
        superseded_count=len(group) - 1,
    )
    return consolidated


def run_full_maintenance(session: Session, *, user_id: int | None = None) -> dict[str, int]:
    """Run all maintenance sweeps. Returns counts of affected memories."""
    results = {
        "decayed": run_decay_sweep(session, user_id=user_id),
        "ttl_expired": run_ttl_sweep(session, user_id=user_id),
        "validity_expired": run_validity_sweep(session, user_id=user_id),
    }
    log.info("memory.maintenance_complete", **results)
    return results
