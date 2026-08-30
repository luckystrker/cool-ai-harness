"""Persistent user-defined macro tools (Phase 4 Agent Constructor)."""

from __future__ import annotations

from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin


class MacroTool(TimestampMixin, table=True):
    """A validated sequence of registered tools exposed as one tool.

    ``steps`` is a list of ``{id, tool_name, arguments}`` objects. String
    argument values may reference ``${input.<name>}`` or
    ``${steps.<id>.output}``; expansion happens immediately before each step.
    """

    __tablename__ = "macro_tools"

    id: int | None = Field(default=None, primary_key=True)
    user_id: int = Field(default=1, foreign_key="users.id", index=True)
    name: str = Field(unique=True, index=True)
    description: str = Field(default="", sa_column=Column(Text))
    input_schema: dict[str, Any] = Field(
        default_factory=lambda: {"type": "object", "properties": {}},
        sa_column=Column("input_schema", JSON),
    )
    steps: list[dict[str, Any]] = Field(default_factory=list, sa_column=Column("steps", JSON))
    is_active: bool = Field(default=True, index=True)
