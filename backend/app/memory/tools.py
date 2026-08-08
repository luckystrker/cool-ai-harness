"""Agent-facing memory tools (Фаза 3a).

Tools the agent can call to interact with long-term and working memory:
- memory_remember: store a new memory
- memory_recall: search memories by query + filters
- memory_forget: archive/delete a memory
- memory_update: update an existing memory
- memory_list: list recent memories
- set_working_memory: update the scratchpad
- get_working_memory: read from the scratchpad
- entity_lookup: resolve a named entity (people, projects, services, tools)
"""

from __future__ import annotations

import json

from pydantic import Field
from sqlmodel import Session

from app.core.db import engine
from app.core.logging import get_logger
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context

log = get_logger(__name__)


# --- Args models ---


class MemoryRememberArgs(ToolArgs):
    content: str = Field(description="The memory content to store")
    memory_type: str = Field(
        default="semantic",
        description="Type: semantic (fact), episodic (event), procedural (how-to), preference",
    )
    importance: float = Field(default=0.5, ge=0.0, le=1.0, description="Importance 0-1")
    tags: list[str] | None = Field(default=None, description="Optional tags for categorization")
    scope: str = Field(
        default="conversation",
        description=(
            "Scope: conversation (default — project/session-specific, stays with this chat), "
            "global (user-wide facts and preferences visible to all agents), "
            "agent (this agent role only). Use 'conversation' for project-specific knowledge, "
            "'global' only for user preferences and facts that apply everywhere."
        ),
    )


class MemoryRecallArgs(ToolArgs):
    query: str = Field(description="Search query to find relevant memories")
    memory_type: str | None = Field(
        default=None, description="Filter by type: semantic, episodic, procedural, preference"
    )
    limit: int = Field(default=5, ge=1, le=20, description="Max results to return")


class MemoryForgetArgs(ToolArgs):
    memory_id: int = Field(description="ID of the memory to forget/archive")
    hard: bool = Field(default=False, description="If true, permanently delete; else archive")


class MemoryUpdateArgs(ToolArgs):
    memory_id: int = Field(description="ID of the memory to update")
    content: str | None = Field(default=None, description="New content (replaces existing)")
    importance: float | None = Field(default=None, ge=0.0, le=1.0, description="New importance")
    tags: list[str] | None = Field(default=None, description="New tags (replaces existing)")


class MemoryListArgs(ToolArgs):
    memory_type: str | None = Field(default=None, description="Filter by type")
    limit: int = Field(default=10, ge=1, le=50, description="Max results")


class SetWorkingMemoryArgs(ToolArgs):
    key: str = Field(description="Key to set in the working memory scratchpad")
    value: str = Field(description="Value to store (string or JSON)")


class GetWorkingMemoryArgs(ToolArgs):
    key: str | None = Field(default=None, description="Key to read; None = entire state")


class EntityLookupArgs(ToolArgs):
    query: str = Field(
        description="Entity name or alias to look up (substring match on name/aliases)"
    )
    entity_type: str | None = Field(
        default=None, description="Filter by type: person, project, service, tool, concept, file"
    )
    limit: int = Field(default=5, ge=1, le=20, description="Max results to return")


# --- Tool implementations ---


async def _memory_remember(
    content: str,
    memory_type: str = "semantic",
    importance: float = 0.5,
    tags: list[str] | None = None,
    scope: str = "conversation",
) -> ToolResult:
    """Store a new memory.

    Default scope is 'conversation' so project-specific knowledge stays with
    the current chat/project instead of polluting the global namespace.
    Use scope='global' explicitly for user-wide preferences and facts.
    """
    from app.memory.service import remember

    ctx = get_run_context()
    # Resolve user_id (MVP: single user).
    with Session(engine) as session:
        from app.agent.service import get_or_create_default_user

        user = get_or_create_default_user(session)
        assert user.id is not None
        memory = remember(
            session,
            user_id=user.id,
            content=content,
            memory_type=memory_type,
            importance=importance,
            tags=tags,
            scope=scope,
            conversation_id=ctx.conversation_id,
            source="agent",
        )
    return ToolResult.ok(
        json.dumps(
            {"id": memory.id, "content": memory.content, "scope": memory.scope, "status": "stored"},
            ensure_ascii=False,
        )
    )


async def _memory_recall(
    query: str,
    memory_type: str | None = None,
    limit: int = 5,
) -> ToolResult:
    """Search memories by query.

    Each result includes provenance (source, confidence, scope) so the agent
    can judge how much to trust a memory and where it came from.
    """
    from app.memory.retrieval import score_memory
    from app.memory.service import recall

    ctx = get_run_context()
    now = None

    # Hybrid vector leg: embed the query best-effort (provider/model resolved
    # from settings; failures degrade to FTS5-only recall).
    query_embedding: list[float] | None = None
    try:
        from app.core.config import get_settings

        settings = get_settings()
        if settings.memory_hybrid_enabled:
            from app.memory.embeddings import VEC_AVAILABLE, vec_table_ready

            if VEC_AVAILABLE and vec_table_ready():
                from app.agent.service import resolve_default_model
                from app.providers import get_provider_for_model

                with Session(engine) as s:
                    model = resolve_default_model(s)
                if model:
                    provider = get_provider_for_model(model)
                    embeddings = await provider.embed(
                        [query], model=settings.memory_embedding_model
                    )
                    if embeddings:
                        query_embedding = embeddings[0]
    except Exception:
        query_embedding = None

    with Session(engine) as session:
        from datetime import UTC, datetime

        from app.agent.service import get_or_create_default_user

        user = get_or_create_default_user(session)
        assert user.id is not None
        memories = recall(
            session,
            user_id=user.id,
            query=query,
            agent_id=ctx.agent_id,
            conversation_id=ctx.conversation_id,
            memory_type=memory_type,
            limit=limit,
            query_embedding=query_embedding,
        )
        now = datetime.now(UTC)
        results = [
            {
                "id": m.id,
                "content": m.content,
                "type": m.memory_type,
                "importance": m.importance,
                "confidence": m.confidence,
                "source": m.source,
                "scope": m.scope,
                "score": score_memory(m, now)["total"],
            }
            for m in memories
        ]
    return ToolResult.ok(json.dumps(results, ensure_ascii=False))


async def _memory_forget(memory_id: int, hard: bool = False) -> ToolResult:
    """Archive or delete a memory."""
    from app.memory.service import forget

    with Session(engine) as session:
        success = forget(session, memory_id, hard=hard)
    if not success:
        return ToolResult.err(f"Memory {memory_id} not found")
    action = "deleted" if hard else "archived"
    return ToolResult.ok(json.dumps({"id": memory_id, "status": action}))


async def _memory_update(
    memory_id: int,
    content: str | None = None,
    importance: float | None = None,
    tags: list[str] | None = None,
) -> ToolResult:
    """Update an existing memory."""
    from app.memory.service import update_memory

    fields: dict = {}
    if content is not None:
        fields["content"] = content
    if importance is not None:
        fields["importance"] = importance
    if tags is not None:
        fields["tags"] = tags

    if not fields:
        return ToolResult.err("No fields to update")

    with Session(engine) as session:
        memory = update_memory(session, memory_id, cap_agent_importance=True, **fields)
    if memory is None:
        return ToolResult.err(f"Memory {memory_id} not found")
    return ToolResult.ok(
        json.dumps({"id": memory.id, "content": memory.content, "status": "updated"})
    )


async def _memory_list(
    memory_type: str | None = None,
    limit: int = 10,
) -> ToolResult:
    """List recent memories."""
    from app.memory.service import list_memories

    with Session(engine) as session:
        from app.agent.service import get_or_create_default_user

        user = get_or_create_default_user(session)
        assert user.id is not None
        memories = list_memories(session, user_id=user.id, memory_type=memory_type, limit=limit)
    results = [
        {
            "id": m.id,
            "content": m.content[:150],
            "type": m.memory_type,
            "importance": m.importance,
            "scope": m.scope,
            "status": m.status,
        }
        for m in memories
    ]
    return ToolResult.ok(json.dumps(results, ensure_ascii=False))


async def _set_working_memory(key: str, value: str) -> ToolResult:
    """Set a key in the working memory scratchpad."""
    from app.memory.service import update_working_memory_state

    ctx = get_run_context()
    if ctx.conversation_id is None:
        return ToolResult.err("No active conversation for working memory")

    # Try to parse value as JSON; fall back to string.
    try:
        parsed_value = json.loads(value)
    except (json.JSONDecodeError, TypeError):
        parsed_value = value

    with Session(engine) as session:
        update_working_memory_state(session, ctx.conversation_id, key, parsed_value)
    return ToolResult.ok(json.dumps({"key": key, "status": "set"}))


async def _get_working_memory(key: str | None = None) -> ToolResult:
    """Read from the working memory scratchpad."""
    from app.memory.service import get_working_memory

    ctx = get_run_context()
    if ctx.conversation_id is None:
        return ToolResult.err("No active conversation for working memory")

    with Session(engine) as session:
        wm = get_working_memory(session, ctx.conversation_id)

    if wm is None:
        return ToolResult.ok(json.dumps({}))

    state = wm.state or {}
    if key is not None:
        value = state.get(key)
        return ToolResult.ok(
            json.dumps({"key": key, "value": value}, ensure_ascii=False, default=str)
        )
    return ToolResult.ok(json.dumps(state, ensure_ascii=False, default=str))


async def _entity_lookup(
    query: str,
    entity_type: str | None = None,
    limit: int = 5,
) -> ToolResult:
    """Look up named entities (people, projects, services, tools, concepts).

    Searches canonical names and aliases. Returns structured records so the
    agent can resolve a reference to a concrete entity with attributes.
    """
    from app.memory.entities import list_entities

    with Session(engine) as session:
        from app.agent.service import get_or_create_default_user

        user = get_or_create_default_user(session)
        assert user.id is not None
        entities = list_entities(
            session,
            user_id=user.id,
            entity_type=entity_type,
            query=query,
            limit=limit,
        )
    results = [
        {
            "id": e.id,
            "name": e.name,
            "type": e.entity_type,
            "aliases": e.aliases or [],
            "attributes": e.attributes or {},
            "description": e.description,
        }
        for e in entities
    ]
    return ToolResult.ok(json.dumps(results, ensure_ascii=False, default=str))


# --- Registration ---


def register_memory_tools() -> None:
    """Register all memory tools on the global registry."""
    register_tool(
        name="memory_remember",
        description=(
            "Store a new long-term memory. Use this to remember important facts, "
            "user preferences, procedures, or events that should persist across sessions."
        ),
        args_model=MemoryRememberArgs,
        func=_memory_remember,
    )
    register_tool(
        name="memory_recall",
        description=(
            "Search long-term memories by query. Returns relevant memories ranked by "
            "relevance, importance, and recency."
        ),
        args_model=MemoryRecallArgs,
        func=_memory_recall,
    )
    register_tool(
        name="memory_forget",
        description="Archive or permanently delete a memory by its ID.",
        args_model=MemoryForgetArgs,
        func=_memory_forget,
    )
    register_tool(
        name="memory_update",
        description="Update the content, importance, or tags of an existing memory.",
        args_model=MemoryUpdateArgs,
        func=_memory_update,
    )
    register_tool(
        name="memory_list",
        description="List recent memories, optionally filtered by type.",
        args_model=MemoryListArgs,
        func=_memory_list,
    )
    register_tool(
        name="set_working_memory",
        description=(
            "Set a key-value pair in the working memory scratchpad for the current "
            "conversation. Useful for tracking goals, hypotheses, and entity states."
        ),
        args_model=SetWorkingMemoryArgs,
        func=_set_working_memory,
    )
    register_tool(
        name="get_working_memory",
        description=(
            "Read from the working memory scratchpad. Pass a key to get a specific "
            "value, or omit key to get the entire state."
        ),
        args_model=GetWorkingMemoryArgs,
        func=_get_working_memory,
    )
    register_tool(
        name="entity_lookup",
        description=(
            "Look up a named entity (person, project, service, tool, concept) by name "
            "or alias. Returns structured records with attributes so you can resolve a "
            "reference to a concrete entity. Use this when a user mentions a specific "
            "thing that may have been recorded before."
        ),
        args_model=EntityLookupArgs,
        func=_entity_lookup,
    )
