"""SQLModel tables for the harness.

Importing this package registers every table on SQLModel.metadata so that
`init_db()` / `create_all()` and Alembic autogenerate can see them.
"""

from __future__ import annotations

from app.models.base import TimestampMixin
from app.models.conversation import Conversation, Message, ToolCall
from app.models.provider import Provider
from app.models.run import AgentRun, RunEvent
from app.models.user import User

__all__ = [
    "AgentRun",
    "Conversation",
    "Message",
    "Provider",
    "RunEvent",
    "TimestampMixin",
    "ToolCall",
    "User",
]
