"""SQLModel tables for the harness.

Importing this package registers every table on SQLModel.metadata so that
`init_db()` / `create_all()` and Alembic autogenerate can see them.

NOTE: Memory models (app.memory.models) are NOT imported here to avoid a
circular import (memory.models -> models.base -> models.__init__ -> memory.models).
They are imported separately in init_db() and the Alembic env.
"""

from __future__ import annotations

from app.models.approval import ApprovalAudit
from app.models.artifact import Artifact
from app.models.base import TimestampMixin
from app.models.budget import Budget, SpendLog
from app.models.conversation import Conversation, Message, ToolCall
from app.models.plan import Plan, PlanStep, PlanTemplate
from app.models.profile import AgentProfile
from app.models.provider import Provider
from app.models.run import AgentRun, RunEvent
from app.models.subagent import SubagentRole, SubagentRun
from app.models.task import ScheduledTask, TaskRun
from app.models.user import User
from app.models.wiki import WikiArticle

__all__ = [
    "AgentProfile",
    "AgentRun",
    "ApprovalAudit",
    "Artifact",
    "Budget",
    "Conversation",
    "Message",
    "Plan",
    "PlanStep",
    "PlanTemplate",
    "Provider",
    "RunEvent",
    "ScheduledTask",
    "SpendLog",
    "SubagentRole",
    "SubagentRun",
    "TaskRun",
    "TimestampMixin",
    "ToolCall",
    "User",
    "WikiArticle",
]
