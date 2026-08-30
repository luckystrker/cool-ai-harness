"""phase 4 completion: multimodal messages, blueprints, and macro tools

Revision ID: 0022
Revises: 0021
"""

from collections.abc import Sequence

import sqlalchemy as sa
from sqlalchemy.dialects import sqlite

from alembic import op

revision: str = "0022"
down_revision: str | Sequence[str] | None = "0021"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    with op.batch_alter_table("messages") as batch_op:
        batch_op.add_column(sa.Column("artifact_ids", sqlite.JSON(), nullable=True))
    with op.batch_alter_table("agent_profiles") as batch_op:
        batch_op.add_column(
            sa.Column("is_shared", sa.Boolean(), nullable=False, server_default=sa.false())
        )
        batch_op.create_index("ix_agent_profiles_is_shared", ["is_shared"], unique=False)
    with op.batch_alter_table("subagent_runs") as batch_op:
        batch_op.add_column(sa.Column("research_run_id", sa.Integer(), nullable=True))
        batch_op.create_foreign_key(
            "fk_subagent_runs_research_run_id_research_runs",
            "research_runs",
            ["research_run_id"],
            ["id"],
        )
        batch_op.create_index(
            "ix_subagent_runs_research_run_id", ["research_run_id"], unique=False
        )

    op.create_table(
        "macro_tools",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("name", sa.String(), nullable=False),
        sa.Column("description", sa.Text(), nullable=False),
        sa.Column("input_schema", sqlite.JSON(), nullable=False),
        sa.Column("steps", sqlite.JSON(), nullable=False),
        sa.Column("is_active", sa.Boolean(), nullable=False),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_macro_tools_user_id", "macro_tools", ["user_id"], unique=False)
    op.create_index("ix_macro_tools_name", "macro_tools", ["name"], unique=True)
    op.create_index("ix_macro_tools_is_active", "macro_tools", ["is_active"], unique=False)


def downgrade() -> None:
    op.drop_index("ix_macro_tools_is_active", table_name="macro_tools")
    op.drop_index("ix_macro_tools_name", table_name="macro_tools")
    op.drop_index("ix_macro_tools_user_id", table_name="macro_tools")
    op.drop_table("macro_tools")
    with op.batch_alter_table("subagent_runs") as batch_op:
        batch_op.drop_index("ix_subagent_runs_research_run_id")
        batch_op.drop_constraint(
            "fk_subagent_runs_research_run_id_research_runs", type_="foreignkey"
        )
        batch_op.drop_column("research_run_id")
    with op.batch_alter_table("agent_profiles") as batch_op:
        batch_op.drop_index("ix_agent_profiles_is_shared")
        batch_op.drop_column("is_shared")
    with op.batch_alter_table("messages") as batch_op:
        batch_op.drop_column("artifact_ids")
