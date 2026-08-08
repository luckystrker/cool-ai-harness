"""Memory lifecycle management — decay, consolidation, TTL enforcement.

Periodic maintenance operations that keep the memory store healthy:
- Decay: reduce importance of unused memories over time.
- Consolidation: merge similar memories to reduce noise.
- Forgetting: archive memories below the importance threshold.
- TTL: auto-archive memories past their expiry date.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta

from sqlmodel import Session, select

from app.core.config import get_settings
from app.core.logging import get_logger
from app.memory.models import (
    MEMORY_STATUS_ACTIVE,
    MEMORY_STATUS_ARCHIVED,
    MEMORY_STATUS_PENDING_CONFIRMATION,
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
        # Pinned and pending-confirmation memories are never decay-archived:
        # pinned items are user-protected; pending items await explicit review
        # (and are cleaned by the auto-reject sweep instead).
        if memory.pinned or memory.status == MEMORY_STATUS_PENDING_CONFIRMATION:
            continue

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
        # Pinned memories never expire via TTL (user-protected).
        if memory.pinned:
            continue
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
        # Pinned memories never expire via validity window (user-protected).
        if memory.pinned:
            continue
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
        tag_key = mem.tags[0] if mem.tags else "untagged"
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
    as superseded. Provenance is preserved: the merged record keeps the
    project key (``_project_key``) and conversation reference of the first
    member, and tags are unioned.
    """
    from app.memory.service import remember

    if not group:
        raise ValueError("Cannot consolidate empty group")

    # Use the first memory's metadata as the base.
    base = group[0]
    importance = merged_importance or max(m.importance for m in group)

    # Merge tags across the group (dedup, cap at 20).
    merged_tags: list[str] = []
    for m in group:
        for t in m.tags or []:
            if t not in merged_tags:
                merged_tags.append(t)
    merged_tags = merged_tags[:20]
    tags_merged: list[str] | None = merged_tags or None

    # Preserve the project key + conversation provenance of the base memory.
    structured = None
    if isinstance(base.structured, dict) and base.structured:
        structured = dict(base.structured)
    conversation_id = base.conversation_id

    # Create the consolidated memory.
    consolidated = remember(
        session,
        user_id=base.user_id,
        content=merged_content,
        memory_type=base.memory_type,
        scope=base.scope,
        agent_id=base.agent_id,
        conversation_id=conversation_id,
        importance=importance,
        confidence=max(m.confidence for m in group),
        source="system",
        tags=tags_merged,
        structured=structured,
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


def run_pending_expiry_sweep(session: Session, *, user_id: int | None = None) -> int:
    """Auto-reject (archive) pending-confirmation memories older than the configured window.

    Unconfirmed agent-extracted memories should not linger forever; after
    ``memory_auto_reject_unconfirmed_days`` days they are archived (still
    recoverable). A window of 0 disables this sweep.

    Returns the number of memories auto-rejected.
    """
    settings = get_settings()
    window_days = settings.memory_auto_reject_unconfirmed_days
    if window_days <= 0:
        return 0

    now = datetime.now(UTC)
    cutoff = now - timedelta(days=window_days)
    stmt = (
        select(MemoryItem)
        .where(MemoryItem.status == MEMORY_STATUS_PENDING_CONFIRMATION)
        .where(MemoryItem.created_at < cutoff)
    )
    if user_id is not None:
        stmt = stmt.where(MemoryItem.user_id == user_id)

    memories = session.exec(stmt).all()
    rejected = 0
    for memory in memories:
        memory.status = MEMORY_STATUS_ARCHIVED
        memory.updated_at = now
        session.add(memory)
        rejected += 1

    if rejected > 0:
        session.commit()
        log.info("memory.pending_expiry_sweep", rejected=rejected)
    return rejected


CONSOLIDATION_MERGE_PROMPT = """\
You merge several overlapping memory entries into one concise entry that \
preserves all distinct facts. Output ONLY valid JSON:
{"content": "<merged memory text>", "importance": 0.0-1.0, "confidence": 0.0-1.0}

Rules:
- Keep every distinct fact; drop repetition.
- The result must stand alone (no references to the input entries).
- No markdown fences, no extra text.
"""


async def _merge_group_with_llm(
    session: Session,
    *,
    provider,
    model: str,
    group: list[MemoryItem],
) -> tuple[str, float] | None:
    """Merge a consolidation group via the LLM. Returns (content, importance)."""
    from app.providers import Message

    entries = "\n".join(f"- {m.content}" for m in group)
    try:
        result = await provider.chat_completion(
            [
                Message(role="system", content=CONSOLIDATION_MERGE_PROMPT),
                Message(role="user", content=f"Memories to merge:\n{entries}"),
            ],
            model=model,
            temperature=0.2,
            max_tokens=600,
        )
        raw = (result.content or "").strip()
        if raw.startswith("```"):
            raw = raw.split("\n", 1)[-1]
            if raw.endswith("```"):
                raw = raw.rsplit("```", 1)[0]
            raw = raw.strip()
        parsed = json.loads(raw)
        content = (parsed.get("content") or "").strip()
        if not content:
            return None
        importance = float(parsed.get("importance", 0.5))
        return content, max(0.0, min(1.0, importance))
    except Exception as exc:
        log.warning("memory.consolidation_llm_failed", error=str(exc))
        return None


async def run_consolidation_sweep(
    session: Session,
    *,
    provider=None,
    model: str | None = None,
    user_id: int | None = None,
) -> dict[str, int]:
    """Merge similar memories into one (C1).

    When ``provider``/``model`` are given and LLM consolidation is enabled,
    groups are merged by the LLM (semantic merge). Otherwise groups are merged
    by simple concatenation. Returns counts: candidates found and groups
    consolidated (bounded by ``memory_consolidation_max_groups``). With
    ``user_id=None`` the sweep covers all users.
    """
    from app.memory.models import MemoryItem

    settings = get_settings()
    threshold = settings.memory_consolidation_threshold
    max_groups = settings.memory_consolidation_max_groups
    llm_enabled = (
        settings.memory_consolidation_llm_enabled and provider is not None and model is not None
    )

    if user_id is not None:
        user_ids: list[int] = [user_id]
    else:
        user_ids = [uid for uid in session.exec(select(MemoryItem.user_id).distinct()).all()]

    total_candidates = 0
    total_consolidated = 0
    for uid in user_ids:
        candidates = find_consolidation_candidates(session, user_id=uid, threshold=threshold)
        # Drop groups that would consolidate into a single leftover entry
        # (a group is only meaningful with >= 2 members).
        groups = [g for g in candidates if len(g) >= 2][:max_groups]
        total_candidates += len(groups)

        for group in groups:
            merged_content: str | None = None
            merged_importance: float | None = None
            if llm_enabled and model is not None:
                merged = await _merge_group_with_llm(
                    session, provider=provider, model=model, group=group
                )
                if merged is not None:
                    merged_content, merged_importance = merged
            if not merged_content:
                # Simple fallback merge (no LLM or LLM failed).
                merged_content = " | ".join(m.content.strip() for m in group if m.content.strip())
            if not merged_content:
                continue
            try:
                consolidate_group(
                    session,
                    group,
                    merged_content,
                    merged_importance=merged_importance,
                )
                total_consolidated += 1
            except Exception as exc:
                log.warning("memory.consolidation_group_failed", error=str(exc))

    log.info(
        "memory.consolidation_sweep",
        users=len(user_ids),
        groups=total_candidates,
        consolidated=total_consolidated,
        llm=llm_enabled,
    )
    return {
        "users": len(user_ids),
        "groups": total_candidates,
        "consolidated": total_consolidated,
    }


def run_full_maintenance(session: Session, *, user_id: int | None = None) -> dict[str, int]:
    """Run all maintenance sweeps. Returns counts of affected memories."""
    results = {
        "decayed": run_decay_sweep(session, user_id=user_id),
        "ttl_expired": run_ttl_sweep(session, user_id=user_id),
        "validity_expired": run_validity_sweep(session, user_id=user_id),
        "pending_expired": run_pending_expiry_sweep(session, user_id=user_id),
    }
    log.info("memory.maintenance_complete", **results)
    return results
