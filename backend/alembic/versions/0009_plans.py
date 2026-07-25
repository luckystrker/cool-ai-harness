"""plans, plan_steps, plan_templates tables (Фаза 2 §1 — Planning Mode)

Revision ID: 0009
Revises: 0008
Create Date: 2026-07-25 10:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa
import sqlmodel

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0009"
down_revision: str | Sequence[str] | None = "0008"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    # --- plans ---
    op.create_table(
        "plans",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("conversation_id", sa.Integer(), nullable=False),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("title", sqlmodel.sql.sqltypes.AutoString(), nullable=True),
        sa.Column("status", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("steps", sa.JSON(), nullable=True),
        sa.Column("metadata_", sa.JSON(), nullable=True),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
        sa.ForeignKeyConstraint(["run_id"], ["agent_runs.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("plans", schema=None) as batch_op:
        batch_op.create_index(batch_op.f("ix_plans_conversation_id"), ["conversation_id"], unique=False)
        batch_op.create_index(batch_op.f("ix_plans_run_id"), ["run_id"], unique=False)
        batch_op.create_index(batch_op.f("ix_plans_status"), ["status"], unique=False)

    # --- plan_steps ---
    op.create_table(
        "plan_steps",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("plan_id", sa.Integer(), nullable=False),
        sa.Column("position", sa.Integer(), nullable=False),
        sa.Column("title", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("description", sa.Text(), nullable=True),
        sa.Column("status", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("depends_on", sa.JSON(), nullable=True),
        sa.Column("tools", sa.JSON(), nullable=True),
        sa.Column("result_summary", sa.Text(), nullable=True),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.ForeignKeyConstraint(["plan_id"], ["plans.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("plan_steps", schema=None) as batch_op:
        batch_op.create_index(batch_op.f("ix_plan_steps_plan_id"), ["plan_id"], unique=False)

    # --- plan_templates ---
    op.create_table(
        "plan_templates",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("name", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("description", sa.Text(), nullable=True),
        sa.Column("steps", sa.JSON(), nullable=False),
        sa.Column("is_builtin", sa.Boolean(), nullable=False),
        sa.PrimaryKeyConstraint("id"),
    )


def downgrade() -> None:
    """Downgrade schema."""
    op.drop_table("plan_templates")

    with op.batch_alter_table("plan_steps", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_plan_steps_plan_id"))
    op.drop_table("plan_steps")

    with op.batch_alter_table("plans", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_plans_status"))
        batch_op.drop_index(batch_op.f("ix_plans_run_id"))
        batch_op.drop_index(batch_op.f("ix_plans_conversation_id"))
    op.drop_table("plans")
