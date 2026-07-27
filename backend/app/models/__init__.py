"""SQLModel tables for the harness.

Importing this package registers every table on SQLModel.metadata so that
`init_db()` / `create_all()` and Alembic autogenerate can see them.
"""

from __future__ import annotations

from app.models.approval import ApprovalAudit
from app.models.artifact import Artifact
from app.models.base import TimestampMixin
from app.models.budget import Budget, SpendLog
from app.models.conversation import Conversation, Message, ToolCall
from app.models.plan import Plan, PlanStep, PlanTemplate
from app.models.provider import Provider
from app.models.run import AgentRun, RunEvent
from app.models.subagent import SubagentRole, SubagentRun
from app.models.user import User

__all__ = [
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
    "SpendLog",
    "SubagentRole",
    "SubagentRun",
    "TimestampMixin",
    "ToolCall",
    "User",
]
