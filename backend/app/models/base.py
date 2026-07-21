"""Common SQLModel mixins and base fields."""

from __future__ import annotations

from datetime import UTC, datetime

from sqlmodel import Field, SQLModel


def _utcnow() -> datetime:
    return datetime.now(UTC)


class TimestampMixin(SQLModel):
    """Provides created_at / updated_at columns."""

    created_at: datetime = Field(default_factory=_utcnow, nullable=False)
    updated_at: datetime = Field(default_factory=_utcnow, nullable=False)
