"""Entity memory service (Фаза 3a — Entity memory).

Normalized named-entity storage with attributes, aliases, and relations.
Entities are the "address book" of long-term memory: they let the agent resolve
a name ("FastAPI", "Alice", "the auth service") to a structured record instead
of relying on free-text recall.

The agent reaches entities only through registered tools (``entity_lookup``) or
the REST API — never by writing the ``entities`` table directly from the loop.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime
from typing import Any

from sqlalchemy import func, or_
from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.memory.models import Entity, EntityRelation, MemoryItem, MemoryItemEntity
from app.providers import LLMProvider, Message

log = get_logger(__name__)

ENTITY_EXTRACTION_SYSTEM_PROMPT = """\
You are an entity extraction system. Analyze the text and extract the named \
entities that would be useful to track across sessions (people, projects, \
services, tools, concepts, files, etc.).

Return a JSON object with this exact structure:
{
  "entities": [
    {
      "name": "canonical name",
      "entity_type": "person|project|service|tool|concept|file|organization|other",
      "aliases": ["alternate name or spelling", "..."],
      "description": "one-line description (optional)",
      "attributes": {"key": "value"}
    }
  ]
}

Rules:
- Only extract entities that recur or matter for future work.
- Prefer a canonical name (e.g. "FastAPI", not "fast api"/"FastAPI framework").
- Include aliases for common alternate spellings.
- If no notable entities, return {"entities": []}.
- Return ONLY valid JSON, no markdown fences or extra text.
"""


# --- CRUD ---


def upsert_entity(
    session: Session,
    *,
    user_id: int,
    name: str,
    entity_type: str = "concept",
    aliases: list[str] | None = None,
    attributes: dict[str, Any] | None = None,
    description: str | None = None,
) -> Entity:
    """Insert or update an entity keyed by (user_id, canonical name).

    On conflict the existing record is merged: aliases/attributes are unioned,
    type/description are replaced if provided.
    """
    existing = session.exec(
        select(Entity)
        .where(Entity.user_id == user_id)
        .where(Entity.name == name)
    ).first()

    if existing is not None:
        # Merge: union aliases/attributes, replace type/description if given.
        if entity_type:
            existing.entity_type = entity_type
        if description:
            existing.description = description
        if aliases:
            merged_aliases = list(set((existing.aliases or []) + aliases))
            existing.aliases = sorted(merged_aliases)
        if attributes:
            merged_attrs = dict(existing.attributes or {})
            merged_attrs.update(attributes)
            existing.attributes = merged_attrs
        existing.updated_at = datetime.now(UTC)
        session.add(existing)
        session.commit()
        session.refresh(existing)
        return existing

    entity = Entity(
        user_id=user_id,
        name=name,
        entity_type=entity_type,
        aliases=aliases,
        attributes=attributes,
        description=description,
    )
    session.add(entity)
    session.commit()
    session.refresh(entity)
    log.info("entity.created", entity_id=entity.id, name=name, entity_type=entity_type)
    return entity


def get_entity(session: Session, entity_id: int) -> Entity | None:
    """Get a single entity by ID."""
    return session.get(Entity, entity_id)


def list_entities(
    session: Session,
    *,
    user_id: int,
    entity_type: str | None = None,
    query: str | None = None,
    limit: int = 100,
    offset: int = 0,
) -> list[Entity]:
    """List entities for a user, optionally filtered by type or name/alias query.

    When ``query`` is given, entities are matched by canonical name OR any alias
    (case-insensitive substring). Because aliases live in a JSON column, the
    name+alias matching is performed in Python after the type filter, so an
    alias-only match is never lost to a SQL name filter.
    """
    stmt = select(Entity).where(Entity.user_id == user_id)
    if entity_type is not None:
        stmt = stmt.where(Entity.entity_type == entity_type)

    # When a query is given we over-fetch (then trim in Python) so that
    # alias-only matches are not dropped by a SQL name filter.
    fetch_limit = limit * 5 if query else limit
    stmt = stmt.order_by(col(Entity.updated_at).desc()).offset(offset).limit(fetch_limit)
    entities = list(session.exec(stmt).all())

    if query:
        q_lower = query.lower()
        # Keep entities that match the name OR any alias; de-duplicate by id.
        matched: list[Entity] = []
        seen: set[int] = set()
        for e in entities:
            if e.id is None or e.id in seen:
                continue
            aliases = [a.lower() for a in (e.aliases or [])]
            if q_lower in e.name.lower() or any(q_lower in a for a in aliases):
                matched.append(e)
                seen.add(e.id)
        return matched[:limit]

    return entities


def update_entity(
    session: Session,
    entity_id: int,
    *,
    name: str | None = None,
    entity_type: str | None = None,
    aliases: list[str] | None = None,
    attributes: dict[str, Any] | None = None,
    description: str | None = None,
) -> Entity | None:
    """Update fields on an entity. Returns None if not found."""
    entity = session.get(Entity, entity_id)
    if entity is None:
        return None
    if name is not None:
        entity.name = name
    if entity_type is not None:
        entity.entity_type = entity_type
    if aliases is not None:
        entity.aliases = aliases
    if attributes is not None:
        entity.attributes = attributes
    if description is not None:
        entity.description = description
    entity.updated_at = datetime.now(UTC)
    session.add(entity)
    session.commit()
    session.refresh(entity)
    return entity


def delete_entity(session: Session, entity_id: int) -> bool:
    """Delete an entity and its link/relation rows. Returns True if deleted."""
    from sqlalchemy import delete as sa_delete

    entity = session.get(Entity, entity_id)
    if entity is None:
        return False
    # Cascade: remove memory links and any relations touching this entity.
    session.execute(
        sa_delete(MemoryItemEntity).where(MemoryItemEntity.entity_id == entity_id)  # type: ignore[arg-type]
    )
    session.execute(
        sa_delete(EntityRelation).where(
            or_(
                EntityRelation.source_entity_id == entity_id,  # type: ignore[arg-type]
                EntityRelation.target_entity_id == entity_id,  # type: ignore[arg-type]
            )
        )
    )
    session.delete(entity)
    session.commit()
    log.info("entity.deleted", entity_id=entity_id)
    return True


# --- Linking ---


def link_memory_to_entity(session: Session, *, memory_id: int, entity_id: int) -> bool:
    """Link a memory to an entity (idempotent). Returns True if a link was added."""
    existing = session.exec(
        select(MemoryItemEntity)
        .where(MemoryItemEntity.memory_id == memory_id)
        .where(MemoryItemEntity.entity_id == entity_id)
    ).first()
    if existing is not None:
        return False
    link = MemoryItemEntity(memory_id=memory_id, entity_id=entity_id)
    session.add(link)
    session.commit()
    return True


def link_entities(
    session: Session,
    *,
    user_id: int,
    source_entity_id: int,
    target_entity_id: int,
    relation_type: str = "related_to",
    attributes: dict[str, Any] | None = None,
) -> EntityRelation:
    """Create a directed relation between two entities (idempotent on the pair+type).."""
    existing = session.exec(
        select(EntityRelation)
        .where(EntityRelation.user_id == user_id)
        .where(EntityRelation.source_entity_id == source_entity_id)
        .where(EntityRelation.target_entity_id == target_entity_id)
        .where(EntityRelation.relation_type == relation_type)
    ).first()
    if existing is not None:
        return existing
    relation = EntityRelation(
        user_id=user_id,
        source_entity_id=source_entity_id,
        target_entity_id=target_entity_id,
        relation_type=relation_type,
        attributes=attributes,
    )
    session.add(relation)
    session.commit()
    session.refresh(relation)
    return relation


def memories_for_entity(session: Session, entity_id: int) -> list[MemoryItem]:
    """Return all active memories linked to an entity."""
    rows = session.exec(
        select(MemoryItemEntity.memory_id).where(MemoryItemEntity.entity_id == entity_id)
    ).all()
    if not rows:
        return []
    memory_ids = [r for r in rows if r is not None]
    if not memory_ids:
        return []
    return list(
        session.exec(
            select(MemoryItem)
            .where(col(MemoryItem.id).in_(memory_ids))
            .order_by(col(MemoryItem.updated_at).desc())
        ).all()
    )


# --- LLM extraction ---


async def extract_entities_from_text(
    session: Session,
    *,
    provider: LLMProvider,
    model: str,
    user_id: int,
    text: str,
    link_memory_id: int | None = None,
) -> list[Entity]:
    """Extract named entities from text via the LLM and upsert them.

    If ``link_memory_id`` is provided, each extracted entity is linked to that
    memory. Returns the list of upserted entities.
    """
    if not text.strip():
        return []

    try:
        result = await provider.chat_completion(
            [
                Message(role="system", content=ENTITY_EXTRACTION_SYSTEM_PROMPT),
                Message(role="user", content=f"Text:\n\n{text}"),
            ],
            model=model,
            temperature=0.1,
            max_tokens=1500,
        )
    except Exception as exc:
        log.warning("entity.extraction_failed", error=str(exc))
        return []

    raw = result.content or ""
    extracted = _parse_entities(raw)
    if not extracted:
        return []

    created: list[Entity] = []
    for ent in extracted:
        name = ent.get("name", "").strip()
        if not name:
            continue
        entity = upsert_entity(
            session,
            user_id=user_id,
            name=name,
            entity_type=ent.get("entity_type", "concept"),
            aliases=ent.get("aliases"),
            attributes=ent.get("attributes"),
            description=ent.get("description"),
        )
        created.append(entity)
        if link_memory_id is not None and entity.id is not None:
            link_memory_to_entity(session, memory_id=link_memory_id, entity_id=entity.id)

    log.info("entity.extraction_complete", extracted_count=len(created))
    return created


def _parse_entities(content: str) -> list[dict[str, Any]]:
    """Parse the LLM's entity-extraction output as JSON. Returns [] on failure."""
    content = content.strip()
    if content.startswith("```"):
        lines = content.split("\n")
        if lines and lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        content = "\n".join(lines)

    try:
        parsed = json.loads(content)
    except json.JSONDecodeError:
        start = content.find("{")
        end = content.rfind("}") + 1
        if start >= 0 and end > start:
            try:
                parsed = json.loads(content[start:end])
            except json.JSONDecodeError:
                return []
        else:
            return []

    entities = parsed.get("entities") if isinstance(parsed, dict) else None
    if not isinstance(entities, list):
        return []
    return [e for e in entities if isinstance(e, dict)]


def count_entities(session: Session, *, user_id: int) -> int:
    """Count entities for a user (for the memory dashboard)."""
    return session.exec(
        select(func.count()).select_from(Entity).where(Entity.user_id == user_id)
    ).one()


def batch_match_entities(
    session: Session,
    *,
    user_id: int,
    words: list[str],
    limit: int = 6,
) -> list[Entity]:
    """Match entities against multiple query words in a single DB round-trip.

    Replaces the per-word ``list_entities`` loop (N+1 pattern) with one query
    that fetches recent entities, then matches all words in Python. Matching
    logic is identical to ``list_entities(query=...)``: case-insensitive
    substring on canonical name or any alias.

    Returns de-duplicated entities (first match wins), capped at ``limit``.
    """
    if not words:
        return []

    # Over-fetch to allow Python-side filtering (same ratio as list_entities).
    fetch_limit = limit * 10
    stmt = (
        select(Entity)
        .where(Entity.user_id == user_id)
        .order_by(col(Entity.updated_at).desc())
        .limit(fetch_limit)
    )
    entities = list(session.exec(stmt).all())

    words_lower = [w.lower() for w in words]
    matched: list[Entity] = []
    seen: set[int] = set()
    for e in entities:
        if e.id is None or e.id in seen:
            continue
        name_lower = e.name.lower()
        aliases_lower = [a.lower() for a in (e.aliases or [])]
        if any(w in name_lower or any(w in a for a in aliases_lower) for w in words_lower):
            matched.append(e)
            seen.add(e.id)
            if len(matched) >= limit:
                break

    return matched
