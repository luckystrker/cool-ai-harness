"""Memory API routes (Фаза 3a).

REST endpoints for managing long-term memories, episodes, and working memory
from the frontend UI.
"""

from __future__ import annotations

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel, Field
from sqlmodel import Session

from app.core.db import engine
from app.memory.models import MEMORY_TYPES, SCOPES

router = APIRouter(prefix="/memory", tags=["memory"])


# --- Request/Response schemas ---


class MemoryCreate(BaseModel):
    content: str = Field(description="Memory content")
    memory_type: str = Field(default="semantic", description="semantic|episodic|procedural|preference")
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


class ExtractRequest(BaseModel):
    conversation_id: int = Field(description="Conversation to extract memories from")


class ExtractResponse(BaseModel):
    status: str
    stored_count: int = 0
    detail: str | None = None


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


# --- Routes ---
# NOTE: Static paths (/episodes, /stats, /extract) must be defined BEFORE
# the dynamic /{memory_id} path, otherwise FastAPI will try to parse
# "episodes"/"stats" as an integer memory_id.


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


@router.get("/stats", response_model=MemoryStatsOut)
def memory_stats() -> MemoryStatsOut:
    """Dashboard stats: counts by type, scope, activity."""
    from sqlmodel import func, select

    from app.memory.models import MEMORY_STATUS_ACTIVE, MEMORY_STATUS_ARCHIVED, Episode, MemoryItem

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
            select(func.count())
            .select_from(Episode)
            .where(Episode.user_id == user_id)
        ).one()

        # Total archived.
        total_archived = session.exec(
            select(func.count())
            .select_from(MemoryItem)
            .where(MemoryItem.user_id == user_id)
            .where(MemoryItem.status == MEMORY_STATUS_ARCHIVED)
        ).one()

    return MemoryStatsOut(
        total_active=total_active,
        by_type=by_type,
        by_scope=by_scope,
        total_episodes=total_episodes,
        total_archived=total_archived,
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


@router.delete("/{memory_id}", status_code=204)
def delete_memory(memory_id: int, hard: bool = Query(default=False)) -> None:
    """Archive (soft-delete) or permanently delete a memory."""
    from app.memory.service import forget

    with Session(engine) as session:
        success = forget(session, memory_id, hard=hard)
    if not success:
        raise HTTPException(status_code=404, detail="Memory not found")
