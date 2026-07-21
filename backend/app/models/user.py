"""User model.

Even though MVP is single-user, every domain table carries a `user_id` so the
schema is multi-tenant ready out of the box (see PLAN.md architectural principles).
"""

from __future__ import annotations

from sqlmodel import Field

from app.models.base import TimestampMixin


class User(TimestampMixin, table=True):
    __tablename__ = "users"

    id: int | None = Field(default=None, primary_key=True)
    # Telegram id (for telegram-based auth) or any external identity.
    external_id: str | None = None
    username: str | None = None
    display_name: str | None = None
    is_active: bool = True
