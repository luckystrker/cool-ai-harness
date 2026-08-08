"""Memory API routes (Фаза 3a).

REST endpoints for managing long-term memories, episodes, working memory, and
named entities from the frontend UI. Also exposes the user-confirmation
workflow (confirm/reject pending memories), pinning, the "why remembered"
explanation, and export.
"""

from __future__ import annotations

from datetime import datetime

from fastapi import APIRouter, HTTPException, Query, Response
from pydantic import BaseModel, Field
from sqlmodel import Session

from app.core.db import engine
from app.memory.models import MEMORY_TYPES, SCOPES

router = APIRouter(prefix="/memory", tags=["memory"])
entities_router = APIRouter(prefix="/entities", tags=["memory"])


# --- Request/Response schemas ---


class MemoryCreate(BaseModel):
    content: str = Field(description="Memory content")
    memory_type: str = Field(
        default="semantic", description="semantic|episodic|procedural|preference"
    )
    scope: str = Field(default="global", description="global|agent|conversation")
    agent_id: int | None = None
    importance: float = Field(default=0.5, ge=0.0, le=1.0)
    confidence: float = Field(default=0.7, ge=0.0, le=1.0)
    tags: list[str] | None = None
    structured: dict | None = None
    ttl_days: int | None = None


class MemoryUpdate(BaseModel):
    content: str | None = None
    memory_type: str | None = None
    scope: str | None = None
    importance: float | None = Field(default=None, ge=0.0, le=1.0)
    confidence: float | None = Field(default=None, ge=0.0, le=1.0)
    status: str | None = None
    tags: list[str] | None = None
    structured: dict | None = None
    ttl_days: int | None = None
    valid_to: str | None = None  # ISO datetime string
    pinned: bool | None = None


class MemoryOut(BaseModel):
    id: int
    user_id: int
    scope: str
    agent_id: int | None
    conversation_id: int | None
    memory_type: str
    content: str
    structured: dict | None
    tags: list[str] | None
    importance: float
    confidence: float
    source: str
    status: str
    pinned: bool
    access_count: int
    created_at: str
    updated_at: str


class EpisodeOut(BaseModel):
    id: int
    user_id: int
    agent_id: int | None
    conversation_id: int | None
    title: str
    summary: str
    outcome: str
    importance: float
    tags: list[str] | None
    created_at: str


class MemoryStatsOut(BaseModel):
    total_active: int
    by_type: dict[str, int]
    by_scope: dict[str, int]
    total_episodes: int
    total_archived: int
    total_pending: int
    total_entities: int


class ExtractRequest(BaseModel):
    conversation_id: int = Field(description="Conversation to extract memories from")


class ExtractResponse(BaseModel):
    status: str
    stored_count: int = 0
    detail: str | None = None


class PinRequest(BaseModel):
    pinned: bool = Field(description="True to pin (protect from decay), False to unpin")


class ExplainOut(BaseModel):
    memory_id: int
    source: str
    scope: str
    status: str
    pinned: bool
    confidence: float
    importance: float
    memory_type: str
    conversation_id: int | None
    agent_id: int | None
    created_at: str
    updated_at: str
    last_accessed_at: str | None
    access_count: int
    score: dict


class EntityCreate(BaseModel):
    name: str = Field(description="Canonical entity name")
    entity_type: str = Field(default="concept")
    aliases: list[str] | None = None
    attributes: dict | None = None
    description: str | None = None


class EntityUpdate(BaseModel):
    name: str | None = None
    entity_type: str | None = None
    aliases: list[str] | None = None
    attributes: dict | None = None
    description: str | None = None


class EntityOut(BaseModel):
    id: int
    user_id: int
    name: str
    entity_type: str
    aliases: list[str] | None
    attributes: dict | None
    description: str | None
    created_at: str
    updated_at: str


# --- Helpers ---


def _get_user_id() -> int:
    """MVP: always user 1."""
    return 1


def _memory_to_out(m) -> MemoryOut:
    return MemoryOut(
        id=m.id,
        user_id=m.user_id,
        scope=m.scope,
        agent_id=m.agent_id,
        conversation_id=m.conversation_id,
        memory_type=m.memory_type,
        content=m.content,
        structured=m.structured,
        tags=m.tags,
        importance=m.importance,
        confidence=m.confidence,
        source=m.source,
        status=m.status,
        pinned=m.pinned,
        access_count=m.access_count,
        created_at=m.created_at.isoformat() if m.created_at else "",
        updated_at=m.updated_at.isoformat() if m.updated_at else "",
    )


def _episode_to_out(e) -> EpisodeOut:
    return EpisodeOut(
        id=e.id,
        user_id=e.user_id,
        agent_id=e.agent_id,
        conversation_id=e.conversation_id,
        title=e.title,
        summary=e.summary,
        outcome=e.outcome,
        importance=e.importance,
        tags=e.tags,
        created_at=e.created_at.isoformat() if e.created_at else "",
    )


def _entity_to_out(e) -> EntityOut:
    return EntityOut(
        id=e.id,
        user_id=e.user_id,
        name=e.name,
        entity_type=e.entity_type,
        aliases=e.aliases,
        attributes=e.attributes,
        description=e.description,
        created_at=e.created_at.isoformat() if e.created_at else "",
        updated_at=e.updated_at.isoformat() if e.updated_at else "",
    )


# --- Routes ---
# NOTE: Static paths (/episodes, /stats, /extract, /pending, /export) must be
# defined BEFORE the dynamic /{memory_id} path, otherwise FastAPI will try to
# parse them as an integer memory_id.


@router.get("", response_model=list[MemoryOut])
def list_memories(
    memory_type: str | None = Query(default=None),
    scope: str | None = Query(default=None),
    status: str | None = Query(default="active"),
    limit: int = Query(default=50, ge=1, le=200),
    offset: int = Query(default=0, ge=0),
) -> list[MemoryOut]:
    """List memories with optional filters."""
    from app.memory.service import list_memories as _list

    with Session(engine) as session:
        memories = _list(
            session,
            user_id=_get_user_id(),
            memory_type=memory_type,
            scope=scope,
            status=status,
            limit=limit,
            offset=offset,
        )
    return [_memory_to_out(m) for m in memories]


@router.post("", response_model=MemoryOut, status_code=201)
def create_memory(body: MemoryCreate) -> MemoryOut:
    """Create a new memory manually."""
    if body.memory_type not in MEMORY_TYPES:
        raise HTTPException(status_code=422, detail=f"Invalid memory_type: {body.memory_type}")
    if body.scope not in SCOPES:
        raise HTTPException(status_code=422, detail=f"Invalid scope: {body.scope}")

    from app.memory.service import remember

    with Session(engine) as session:
        memory = remember(
            session,
            user_id=_get_user_id(),
            content=body.content,
            memory_type=body.memory_type,
            scope=body.scope,
            agent_id=body.agent_id,
            importance=body.importance,
            confidence=body.confidence,
            tags=body.tags,
            structured=body.structured,
            ttl_days=body.ttl_days,
            source="user_explicit",
        )
    return _memory_to_out(memory)


@router.get("/episodes", response_model=list[EpisodeOut])
def list_episodes(
    agent_id: int | None = Query(default=None),
    limit: int = Query(default=20, ge=1, le=100),
) -> list[EpisodeOut]:
    """List episodic memories."""
    from app.memory.service import list_episodes as _list

    with Session(engine) as session:
        episodes = _list(session, user_id=_get_user_id(), agent_id=agent_id, limit=limit)
    return [_episode_to_out(e) for e in episodes]


@router.get("/pending", response_model=list[MemoryOut])
def list_pending(
    limit: int = Query(default=100, ge=1, le=500),
    offset: int = Query(default=0, ge=0),
) -> list[MemoryOut]:
    """List memories awaiting user confirmation (agent-extracted, not yet reviewed)."""
    from app.memory.service import list_pending as _list

    with Session(engine) as session:
        pending = _list(session, user_id=_get_user_id(), limit=limit, offset=offset)
    return [_memory_to_out(m) for m in pending]


@router.get("/export")
def export_memories(
    format: str = Query(default="json", description="json|markdown"),
    include_archived: bool = Query(default=False),
) -> Response:
    """Export the user's memories as a downloadable file."""
    from app.memory.service import export_memories as _export

    fmt = format.lower()
    if fmt not in {"json", "markdown"}:
        raise HTTPException(status_code=422, detail="format must be 'json' or 'markdown'")

    with Session(engine) as session:
        data = _export(
            session,
            user_id=_get_user_id(),
            fmt=fmt,
            include_archived=include_archived,
        )

    if fmt == "json":
        return Response(
            content=data,
            media_type="application/json",
            headers={"Content-Disposition": "attachment; filename=memories.json"},
        )
    return Response(
        content=data,
        media_type="text/markdown; charset=utf-8",
        headers={"Content-Disposition": "attachment; filename=memories.md"},
    )


@router.get("/stats", response_model=MemoryStatsOut)
def memory_stats() -> MemoryStatsOut:
    """Dashboard stats: counts by type, scope, pending, entities, activity."""
    from sqlmodel import func, select

    from app.memory.entities import count_entities
    from app.memory.models import (
        MEMORY_STATUS_ACTIVE,
        MEMORY_STATUS_ARCHIVED,
        MEMORY_STATUS_PENDING_CONFIRMATION,
        Episode,
        MemoryItem,
    )

    with Session(engine) as session:
        user_id = _get_user_id()

        # Total active.
        total_active = session.exec(
            select(func.count())
            .select_from(MemoryItem)
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
        ).one()

        # By type.
        by_type_rows = session.exec(
            select(MemoryItem.memory_type, func.count())
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
            .group_by(MemoryItem.memory_type)
        ).all()
        by_type = {row[0]: row[1] for row in by_type_rows}

        # By scope.
        by_scope_rows = session.exec(
            select(MemoryItem.scope, func.count())
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ACTIVE)
            .group_by(MemoryItem.scope)
        ).all()
        by_scope = {row[0]: row[1] for row in by_scope_rows}

        # Total episodes.
        total_episodes = session.exec(
            select(func.count()).select_from(Episode).where(Episode.user_id == user_id)
        ).one()

        # Total archived.
        total_archived = session.exec(
            select(func.count())
            .select_from(MemoryItem)
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ARCHIVED)
        ).one()

        # Total pending confirmation.
        total_pending = session.exec(
            select(func.count())
            .select_from(MemoryItem)
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_PENDING_CONFIRMATION)
        ).one()

        # Total entities.
        total_entities = count_entities(session, user_id=user_id)

    return MemoryStatsOut(
        total_active=total_active,
        by_type=by_type,
        by_scope=by_scope,
        total_episodes=total_episodes,
        total_archived=total_archived,
        total_pending=total_pending,
        total_entities=total_entities,
    )


@router.post("/extract", response_model=ExtractResponse)
async def trigger_extraction(body: ExtractRequest) -> ExtractResponse:
    """Trigger memory extraction for a conversation.

    This is a synchronous extraction that runs immediately. For production,
    this would be a background task.
    """
    from app.agent.service import load_history, resolve_default_model
    from app.memory.extractor import extract_memories_from_conversation
    from app.providers import get_provider_for_model

    with Session(engine) as session:
        user_id = _get_user_id()
        model = resolve_default_model(session)
        if model is None:
            return ExtractResponse(status="error", detail="No model configured")

        messages = load_history(session, body.conversation_id)
        if len(messages) < 4:
            return ExtractResponse(status="skipped", detail="Too few messages")

    provider = get_provider_for_model(model)

    with Session(engine) as session:
        result = await extract_memories_from_conversation(
            session,
            provider=provider,
            model=model,
            user_id=user_id,
            conversation_id=body.conversation_id,
            messages=messages,
        )

    if result.get("skipped"):
        return ExtractResponse(status="skipped", detail=result.get("reason"))

    return ExtractResponse(
        status="completed",
        stored_count=result.get("stored_count", 0),
    )


class ConsolidateResponse(BaseModel):
    status: str
    groups: int = 0
    consolidated: int = 0
    detail: str | None = None


@router.post("/consolidate", response_model=ConsolidateResponse)
async def trigger_consolidation() -> ConsolidateResponse:
    """Run the consolidation sweep now (merge similar memories, LLM-assisted)."""
    from app.agent.service import resolve_default_model
    from app.memory.lifecycle import run_consolidation_sweep
    from app.providers import get_provider_for_model

    with Session(engine) as session:
        model = resolve_default_model(session)
    if model is None:
        return ConsolidateResponse(status="error", detail="No model configured")

    provider = get_provider_for_model(model)

    with Session(engine) as session:
        results = await run_consolidation_sweep(
            session, provider=provider, model=model, user_id=_get_user_id()
        )

    return ConsolidateResponse(
        status="completed",
        groups=results.get("groups", 0),
        consolidated=results.get("consolidated", 0),
    )


# --- Dynamic routes (/{memory_id}) — must come AFTER static paths ---


@router.get("/{memory_id}", response_model=MemoryOut)
def get_memory(memory_id: int) -> MemoryOut:
    """Get a single memory by ID."""
    from app.memory.service import get_memory as _get

    with Session(engine) as session:
        memory = _get(session, memory_id)
    if memory is None:
        raise HTTPException(status_code=404, detail="Memory not found")
    return _memory_to_out(memory)


@router.get("/{memory_id}/explain", response_model=ExplainOut)
def explain_memory(memory_id: int) -> ExplainOut:
    """Explain why a memory is remembered (source, score breakdown, lifecycle)."""
    from app.memory.service import explain_memory as _explain

    with Session(engine) as session:
        explanation = _explain(session, memory_id)
    if explanation is None:
        raise HTTPException(status_code=404, detail="Memory not found")

    # Serialize datetimes for the response.
    def _iso(value):
        return value.isoformat() if isinstance(value, datetime) else (value or None)

    return ExplainOut(
        memory_id=explanation["memory_id"],
        source=explanation["source"],
        scope=explanation["scope"],
        status=explanation["status"],
        pinned=explanation["pinned"],
        confidence=explanation["confidence"],
        importance=explanation["importance"],
        memory_type=explanation["memory_type"],
        conversation_id=explanation["conversation_id"],
        agent_id=explanation["agent_id"],
        created_at=_iso(explanation["created_at"]),
        updated_at=_iso(explanation["updated_at"]),
        last_accessed_at=_iso(explanation["last_accessed_at"]),
        access_count=explanation["access_count"],
        score=explanation["score"],
    )


@router.patch("/{memory_id}", response_model=MemoryOut)
def update_memory(memory_id: int, body: MemoryUpdate) -> MemoryOut:
    """Update a memory."""
    from app.memory.service import update_memory as _update

    fields = body.model_dump(exclude_none=True)
    if not fields:
        raise HTTPException(status_code=422, detail="No fields to update")

    with Session(engine) as session:
        memory = _update(session, memory_id, **fields)
    if memory is None:
        raise HTTPException(status_code=404, detail="Memory not found")
    return _memory_to_out(memory)


@router.post("/{memory_id}/confirm", response_model=MemoryOut)
def confirm_memory(memory_id: int) -> MemoryOut:
    """Confirm a pending memory (promote to active)."""
    from app.memory.service import confirm_memory as _confirm

    with Session(engine) as session:
        memory = _confirm(session, memory_id)
    if memory is None:
        raise HTTPException(status_code=404, detail="Memory not found")
    return _memory_to_out(memory)


@router.post("/{memory_id}/reject", status_code=204)
def reject_memory(memory_id: int) -> None:
    """Reject a pending memory (archive it)."""
    from app.memory.service import reject_memory as _reject

    with Session(engine) as session:
        success = _reject(session, memory_id)
    if not success:
        raise HTTPException(status_code=404, detail="Memory not found")


@router.post("/{memory_id}/pin", response_model=MemoryOut)
def pin_memory(memory_id: int, body: PinRequest) -> MemoryOut:
    """Pin or unpin a memory (pinned memories are protected from decay/TTL)."""
    from app.memory.service import pin_memory as _pin

    with Session(engine) as session:
        memory = _pin(session, memory_id, pinned=body.pinned)
    if memory is None:
        raise HTTPException(status_code=404, detail="Memory not found")
    return _memory_to_out(memory)


@router.delete("/{memory_id}", status_code=204)
def delete_memory(memory_id: int, hard: bool = Query(default=False)) -> None:
    """Archive (soft-delete) or permanently delete a memory."""
    from app.memory.service import forget

    with Session(engine) as session:
        success = forget(session, memory_id, hard=hard)
    if not success:
        raise HTTPException(status_code=404, detail="Memory not found")


# --- Entities CRUD ---


@entities_router.get("", response_model=list[EntityOut])
def list_entities(
    entity_type: str | None = Query(default=None),
    query: str | None = Query(default=None),
    limit: int = Query(default=100, ge=1, le=500),
    offset: int = Query(default=0, ge=0),
) -> list[EntityOut]:
    """List named entities for the user, optionally filtered by type or name/alias."""
    from app.memory.entities import list_entities as _list

    with Session(engine) as session:
        entities = _list(
            session,
            user_id=_get_user_id(),
            entity_type=entity_type,
            query=query,
            limit=limit,
            offset=offset,
        )
    return [_entity_to_out(e) for e in entities]


@entities_router.post("", response_model=EntityOut, status_code=201)
def create_entity(body: EntityCreate) -> EntityOut:
    """Create or update (upsert) a named entity."""
    from app.memory.entities import upsert_entity

    with Session(engine) as session:
        entity = upsert_entity(
            session,
            user_id=_get_user_id(),
            name=body.name,
            entity_type=body.entity_type,
            aliases=body.aliases,
            attributes=body.attributes,
            description=body.description,
        )
    return _entity_to_out(entity)


@entities_router.get("/{entity_id}", response_model=EntityOut)
def get_entity(entity_id: int) -> EntityOut:
    """Get a single entity by ID."""
    from app.memory.entities import get_entity as _get

    with Session(engine) as session:
        entity = _get(session, entity_id)
    if entity is None:
        raise HTTPException(status_code=404, detail="Entity not found")
    return _entity_to_out(entity)


@entities_router.patch("/{entity_id}", response_model=EntityOut)
def update_entity(entity_id: int, body: EntityUpdate) -> EntityOut:
    """Update an entity."""
    from app.memory.entities import update_entity as _update

    fields = body.model_dump(exclude_none=True)
    if not fields:
        raise HTTPException(status_code=422, detail="No fields to update")

    with Session(engine) as session:
        entity = _update(session, entity_id, **fields)
    if entity is None:
        raise HTTPException(status_code=404, detail="Entity not found")
    return _entity_to_out(entity)


@entities_router.delete("/{entity_id}", status_code=204)
def delete_entity(entity_id: int) -> None:
    """Delete an entity and its links/relations."""
    from app.memory.entities import delete_entity as _delete

    with Session(engine) as session:
        success = _delete(session, entity_id)
    if not success:
        raise HTTPException(status_code=404, detail="Entity not found")
