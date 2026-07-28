"""Memory models (Фаза 3a — Long-term + Working memory).

Tables:
- ``MemoryItem``: core long-term memory record (semantic, episodic, procedural, preference).
- ``Episode``: episodic memory — session/run summaries with outcome tracking.
- ``WorkingMemory``: per-conversation session state (scratchpad, rolling summary).
- ``Entity``: named entity with attributes and aliases (entity memory).
- ``EntityRelation``: directed relationship between two entities.
- ``MemoryItemEntity``: link table — which memories reference which entities.

Scope model (multi-agent ready):
- ``global``: visible to all agents/conversations for a user.
- ``agent``: visible only when the active role/personality matches ``agent_id``.
- ``conversation``: visible only within the originating conversation.

Confirmation model:
- Memories created by the agent / agent-extraction land in ``pending_confirmation``
  status and are excluded from recall/context until the user confirms them
  (status → ``active``) or rejects them (status → ``archived``).
- ``user_explicit`` and ``system`` sources are stored directly as ``active``.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from sqlalchemy import Column, Text, UniqueConstraint
from sqlalchemy.types import JSON
from sqlmodel import Field, SQLModel

from app.models.base import TimestampMixin, _utcnow

# --- Constants ---

# Memory scopes.
SCOPE_GLOBAL = "global"
SCOPE_AGENT = "agent"
SCOPE_CONVERSATION = "conversation"

SCOPES = frozenset({SCOPE_GLOBAL, SCOPE_AGENT, SCOPE_CONVERSATION})

# Memory types.
MEMORY_TYPE_SEMANTIC = "semantic"
MEMORY_TYPE_EPISODIC = "episodic"
MEMORY_TYPE_PROCEDURAL = "procedural"
MEMORY_TYPE_PREFERENCE = "preference"

MEMORY_TYPES = frozenset(
    {MEMORY_TYPE_SEMANTIC, MEMORY_TYPE_EPISODIC, MEMORY_TYPE_PROCEDURAL, MEMORY_TYPE_PREFERENCE}
)

# Memory statuses.
MEMORY_STATUS_ACTIVE = "active"
MEMORY_STATUS_ARCHIVED = "archived"
MEMORY_STATUS_SUPERSEDED = "superseded"
MEMORY_STATUS_DELETED = "deleted"
# Memories created by agent/agent_extraction sources land here first; they are
# excluded from recall/context until the user confirms (→ active) or rejects (→ archived).
MEMORY_STATUS_PENDING_CONFIRMATION = "pending_confirmation"

MEMORY_STATUSES = frozenset(
    {
        MEMORY_STATUS_ACTIVE,
        MEMORY_STATUS_ARCHIVED,
        MEMORY_STATUS_SUPERSEDED,
        MEMORY_STATUS_DELETED,
        MEMORY_STATUS_PENDING_CONFIRMATION,
    }
)

# Memory sources.
MEMORY_SOURCE_USER_EXPLICIT = "user_explicit"
MEMORY_SOURCE_AGENT_EXTRACTION = "agent_extraction"
MEMORY_SOURCE_AGENT = "agent"
MEMORY_SOURCE_SYSTEM = "system"

# Episode outcomes.
EPISODE_OUTCOME_SUCCESS = "success"
EPISODE_OUTCOME_FAILURE = "failure"
EPISODE_OUTCOME_PARTIAL = "partial"
EPISODE_OUTCOME_UNKNOWN = "unknown"


class MemoryItem(TimestampMixin, table=True):
    """Core long-term memory record.

    Stores semantic facts, episodic summaries, procedural knowledge, and
    user/agent preferences with a three-level namespace (global/agent/conversation).
    """

    __tablename__ = "memory_items"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)

    # --- Namespace / Scope ---
    # "global" | "agent" | "conversation"
    scope: str = Field(default=SCOPE_GLOBAL, index=True)
    # For scope="agent": which agent role/personality owns this memory.
    # NULL for global scope. Links to subagent_roles.id (or future agent_profiles.id).
    agent_id: int | None = Field(default=None, index=True)
    # For scope="conversation": originating conversation (provenance, not visibility).
    conversation_id: int | None = Field(default=None, foreign_key="conversations.id")

    # --- Content ---
    # "semantic" | "episodic" | "procedural" | "preference"
    memory_type: str = Field(default=MEMORY_TYPE_SEMANTIC, index=True)
    content: str = Field(sa_column=Column(Text))
    # Optional structured payload (key/value for preferences, command for procedures).
    structured: dict[str, Any] | None = Field(default=None, sa_column=Column("structured", JSON))
    tags: list[str] | None = Field(default=None, sa_column=Column("tags", JSON))

    # --- Metadata ---
    importance: float = Field(default=0.5)  # 0..1, how important this memory is
    confidence: float = Field(default=0.7)  # 0..1, how certain we are
    source: str = Field(default=MEMORY_SOURCE_AGENT)  # who/what created this
    status: str = Field(default=MEMORY_STATUS_ACTIVE, index=True)
    # Conflict resolution: this memory replaces another (points to memory_items.id).
    supersedes_id: int | None = None

    # --- Lifecycle ---
    access_count: int = 0
    last_accessed_at: datetime | None = None
    ttl_days: int | None = None  # NULL = no expiry
    valid_from: datetime | None = Field(default_factory=_utcnow)
    valid_to: datetime | None = None  # for facts that can become stale
    # User-pinned memories are protected from decay/TTL sweeps.
    pinned: bool = Field(default=False, index=True)


class Episode(TimestampMixin, table=True):
    """Episodic memory — session/run summaries with outcome tracking.

    Captures what happened during a conversation or agent run, enabling the
    agent to learn from past experiences and avoid repeating mistakes.
    """

    __tablename__ = "episodes"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    agent_id: int | None = Field(default=None, index=True)
    conversation_id: int | None = Field(default=None, foreign_key="conversations.id")
    run_id: int | None = Field(default=None, foreign_key="agent_runs.id")

    title: str
    summary: str = Field(sa_column=Column(Text))
    outcome: str = Field(default=EPISODE_OUTCOME_UNKNOWN)  # success|failure|partial|unknown
    importance: float = Field(default=0.5)
    tags: list[str] | None = Field(default=None, sa_column=Column("tags", JSON))
    # Related entities (files, services, concepts) referenced in this episode.
    related_entities: list[str] | None = Field(
        default=None, sa_column=Column("related_entities", JSON)
    )
    started_at: datetime | None = None
    ended_at: datetime | None = None


class WorkingMemory(TimestampMixin, table=True):
    """Per-conversation session state (short-term / working memory).

    Stores the structured scratchpad (goal, hypotheses, plan, variables, entity
    states) and a rolling summary of older messages for context compression.
    """

    __tablename__ = "working_memory"

    id: int | None = Field(default=None, primary_key=True)
    conversation_id: int = Field(foreign_key="conversations.id", index=True, unique=True)

    # Structured scratchpad (goal, hypotheses, plan, variables, entity states).
    state: dict[str, Any] = Field(default_factory=dict, sa_column=Column("state", JSON))
    # Rolling summary of older messages (compressed context).
    summary: str | None = Field(default=None, sa_column=Column(Text))
    # Message ID up to which the summary covers.
    summary_up_to_message_id: int | None = None
    # Token count estimate of the current context.
    token_estimate: int | None = None


# --- Entity memory (named entities with attributes, aliases, and relations) ---


class Entity(TimestampMixin, table=True):
    """Named entity memory — people, projects, services, concepts, etc.

    Entities are normalized records with attributes and aliases. They link to
    memories (and indirectly to episodes) so the agent can resolve "X" to a
    structured record instead of relying on free-text recall.
    """

    __tablename__ = "entities"
    # Canonical name is unique per user (upsert merges on this pair).
    __table_args__ = (UniqueConstraint("user_id", "name", name="uq_entities_user_id_name"),)

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    # Canonical name (unique per user).
    name: str = Field(index=True)
    # e.g. "person", "project", "service", "tool", "concept", "file".
    entity_type: str = Field(default="concept", index=True)
    # Alternate names / spellings used to refer to this entity.
    aliases: list[str] | None = Field(default=None, sa_column=Column("aliases", JSON))
    # Free-form structured attributes {key: value}.
    attributes: dict[str, Any] | None = Field(default=None, sa_column=Column("attributes", JSON))
    description: str | None = Field(default=None, sa_column=Column(Text))


class EntityRelation(TimestampMixin, table=True):
    """Directed relationship between two entities.

    e.g. (User, "works_on", Project), (Service, "depends_on", Service).
    """

    __tablename__ = "entity_relations"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(foreign_key="users.id", index=True)
    source_entity_id: int = Field(foreign_key="entities.id", index=True)
    target_entity_id: int = Field(foreign_key="entities.id", index=True)
    relation_type: str = Field(default="related_to")
    attributes: dict[str, Any] | None = Field(default=None, sa_column=Column("attributes", JSON))


class MemoryItemEntity(SQLModel, table=True):
    """Link table — which memories reference which entities.

    A memory may mention several entities; an entity may be referenced by many
    memories. Many-to-many without timestamps (pure join).
    """

    __tablename__ = "memory_item_entities"

    memory_id: int = Field(foreign_key="memory_items.id", primary_key=True)
    entity_id: int = Field(foreign_key="entities.id", primary_key=True)
