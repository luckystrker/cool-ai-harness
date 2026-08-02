"""research_runs

Adds the research_runs table for the deep research workflow (Фаза 4).

Revision ID: 4869d4c0322a
Revises: 0020
Create Date: 2026-08-02 21:17:33.588619

"""

from collections.abc import Sequence

import sqlalchemy as sa
from sqlalchemy.dialects import sqlite

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "4869d4c0322a"
down_revision: str | Sequence[str] | None = "0020"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.create_table(
        "research_runs",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("conversation_id", sa.Integer(), nullable=True),
        sa.Column("parent_task_run_id", sa.Integer(), nullable=True),
        sa.Column("topic", sa.Text(), nullable=True),
        sa.Column("depth", sa.Integer(), nullable=False),
        sa.Column("model", sa.String(), nullable=True),
        sa.Column("status", sa.String(), nullable=False),
        sa.Column("sub_questions", sqlite.JSON(), nullable=True),
        sa.Column("sources", sqlite.JSON(), nullable=True),
        sa.Column("citations", sqlite.JSON(), nullable=True),
        sa.Column("report_markdown", sa.Text(), nullable=True),
        sa.Column("report_artifact_id", sa.Integer(), nullable=True),
        sa.Column("usage", sqlite.JSON(), nullable=True),
        sa.Column("error", sa.Text(), nullable=True),
        sa.Column("input_hash", sa.String(), nullable=True),
        sa.Column("finished_at", sa.DateTime(), nullable=True),
        sa.ForeignKeyConstraint(
            ["conversation_id"],
            ["conversations.id"],
        ),
        sa.ForeignKeyConstraint(
            ["parent_task_run_id"],
            ["task_runs.id"],
        ),
        sa.ForeignKeyConstraint(
            ["report_artifact_id"],
            ["artifacts.id"],
        ),
        sa.ForeignKeyConstraint(
            ["user_id"],
            ["users.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("research_runs", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_research_runs_conversation_id"), ["conversation_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_research_runs_input_hash"), ["input_hash"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_research_runs_parent_task_run_id"), ["parent_task_run_id"], unique=False
        )
        batch_op.create_index(
            batch_op.f("ix_research_runs_report_artifact_id"), ["report_artifact_id"], unique=False
        )
        batch_op.create_index(batch_op.f("ix_research_runs_status"), ["status"], unique=False)
        batch_op.create_index(batch_op.f("ix_research_runs_user_id"), ["user_id"], unique=False)


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("research_runs", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_research_runs_user_id"))
        batch_op.drop_index(batch_op.f("ix_research_runs_status"))
        batch_op.drop_index(batch_op.f("ix_research_runs_report_artifact_id"))
        batch_op.drop_index(batch_op.f("ix_research_runs_parent_task_run_id"))
        batch_op.drop_index(batch_op.f("ix_research_runs_input_hash"))
        batch_op.drop_index(batch_op.f("ix_research_runs_conversation_id"))

    op.drop_table("research_runs")
