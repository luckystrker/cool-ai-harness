"""Wiki / Knowledge Base article model (Фаза 3a §3).

A ``WikiArticle`` is an organized knowledge entry — distinct from chaotic
memory items. Articles support Markdown content, categories, tags, and
full-text search via FTS5.

Facts from semantic memory can be "promoted" into the KB upon user
confirmation, giving them permanent, organized storage.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin


class WikiArticle(TimestampMixin, table=True):
    """A structured knowledge base article."""

    __tablename__ = "wiki_articles"

    id: int | None = Field(default=None, primary_key=True)
    # Article title (unique within a category).
    title: str = Field(index=True)
    # Markdown content body.
    content: str = Field(default="", sa_column=Column(Text))
    # Category for organization (e.g. "project", "research", "campaign", "how-to").
    category: str = Field(default="general", index=True)
    # Tags for filtering and discovery.
    tags: list[str] = Field(default_factory=list, sa_column=Column("tags", JSON))
    # Source provenance: "manual", "agent", "memory_promotion".
    source: str = Field(default="manual")
    # If promoted from memory, the original memory_item id.
    source_memory_id: int | None = None
    # Which user owns this article (multi-user readiness).
    user_id: int | None = Field(default=None, foreign_key="users.id", index=True)
    # Project scope key (matches memory's _project_key for visibility).
    project_key: str | None = Field(default=None, index=True)
    # Whether the article is pinned (protected from cleanup).
    is_pinned: bool = False
    # Soft-delete flag.
    is_archived: bool = Field(default=False, index=True)
    # Version counter for optimistic concurrency.
    version: int = Field(default=1)
    # Arbitrary metadata.
    metadata_: dict[str, Any] | None = Field(default=None, sa_column=Column("metadata_", JSON))
