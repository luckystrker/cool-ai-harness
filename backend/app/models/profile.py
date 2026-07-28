"""Agent Profile model (Фаза 3a §2 — Multi-personality agents).

An ``AgentProfile`` is a persistent personality configuration: system prompt,
preferred model, tool/skill whitelists, runtime settings, and a memory
namespace (via ``id`` used as ``agent_id`` in the memory subsystem).

Profiles can be assigned to conversations, switched in the chat UI, and
invoked as subagents by other profiles.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin


class AgentProfile(TimestampMixin, table=True):
    """Reusable agent personality/profile definition."""

    __tablename__ = "agent_profiles"

    id: int | None = Field(default=None, primary_key=True)
    # Human-readable name, e.g. "Coder", "DM".
    name: str = Field(index=True)
    # URL-safe unique identifier, e.g. "coder", "dm".
    slug: str = Field(unique=True, index=True)
    description: str | None = Field(default=None, sa_column=Column(Text))
    # Profile-specific system prompt injected as the base prompt.
    system_prompt: str | None = Field(default=None, sa_column=Column(Text))
    # Preferred model (None = inherit conversation/global default).
    model: str | None = None
    # Tool whitelist (None = all registered tools available).
    tool_names: list[str] | None = Field(default=None, sa_column=Column("tool_names", JSON))
    # Skill whitelist (None = all skills available).
    skill_names: list[str] | None = Field(default=None, sa_column=Column("skill_names", JSON))
    # Runtime settings: temperature, max_tokens, max_iterations, capability_policy, etc.
    settings: dict[str, Any] | None = Field(default=None, sa_column=Column("settings", JSON))
    # Hex color for the UI avatar badge, e.g. "#3B82F6".
    avatar_color: str | None = None
    # Built-in presets ship with the app and cannot be deleted.
    is_builtin: bool = False
    # Soft-disable: inactive profiles are hidden from the switcher.
    is_active: bool = Field(default=True, index=True)
