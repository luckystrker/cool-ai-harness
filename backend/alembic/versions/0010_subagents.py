"""subagent_roles and subagent_runs tables (Фаза 2 §5 — Subagents)

Revision ID: 0010
Revises: 0009
Create Date: 2026-07-27 10:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0010"
down_revision: str | Sequence[str] | None = "0009"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    # --- subagent_roles ---
    op.create_table(
        "subagent_roles",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("name", sa.String(), nullable=False),
        sa.Column("description", sa.Text(), nullable=True),
        sa.Column("system_prompt", sa.Text(), nullable=True),
        sa.Column("model", sa.String(), nullable=True),
        sa.Column("tool_names", sa.JSON(), nullable=True),
        sa.Column("capability_policy", sa.JSON(), nullable=True),
        sa.Column("max_iterations", sa.Integer(), nullable=False, server_default="10"),
        sa.Column("max_cost_usd", sa.Float(), nullable=True),
        sa.Column("is_builtin", sa.Boolean(), nullable=False, server_default=sa.text("0")),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_subagent_roles_name", "subagent_roles", ["name"])

    # --- subagent_runs ---
    op.create_table(
        "subagent_runs",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("role_id", sa.Integer(), nullable=True),
        sa.Column("parent_conversation_id", sa.Integer(), nullable=False),
        sa.Column("parent_run_id", sa.Integer(), nullable=True),
        sa.Column("conversation_id", sa.Integer(), nullable=False),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("name", sa.String(), nullable=True),
        sa.Column("prompt", sa.Text(), nullable=False),
        sa.Column("status", sa.String(), nullable=False, server_default="queued"),
        sa.Column("result_summary", sa.Text(), nullable=True),
        sa.Column("usage", sa.JSON(), nullable=True),
        sa.Column("error", sa.Text(), nullable=True),
        sa.Column("started_at", sa.DateTime(), nullable=False),
        sa.Column("finished_at", sa.DateTime(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["role_id"], ["subagent_roles.id"]),
        sa.ForeignKeyConstraint(["parent_conversation_id"], ["conversations.id"]),
        sa.ForeignKeyConstraint(["parent_run_id"], ["agent_runs.id"]),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
        sa.ForeignKeyConstraint(["run_id"], ["agent_runs.id"]),
    )
    op.create_index("ix_subagent_runs_role_id", "subagent_runs", ["role_id"])
    op.create_index(
        "ix_subagent_runs_parent_conversation_id", "subagent_runs", ["parent_conversation_id"]
    )
    op.create_index("ix_subagent_runs_parent_run_id", "subagent_runs", ["parent_run_id"])
    op.create_index("ix_subagent_runs_status", "subagent_runs", ["status"])


def downgrade() -> None:
    """Downgrade schema."""
    op.drop_table("subagent_runs")
    op.drop_table("subagent_roles")
