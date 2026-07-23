"""approval_audits table and capability_policy column

Revision ID: 0003
Revises: 0002
Create Date: 2026-07-23 22:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa
import sqlmodel

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0003"
down_revision: str | Sequence[str] | None = "0002"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    # Approval audit trail table (Фаза 1.5 §2).
    op.create_table(
        "approval_audits",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("conversation_id", sa.Integer(), nullable=False),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("call_id", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("tool_name", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("arguments", sa.JSON(), nullable=True),
        sa.Column("approved", sa.Boolean(), nullable=False),
        sa.Column("decision_source", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("decided_by", sqlmodel.sql.sqltypes.AutoString(), nullable=True),
        sa.Column("reason", sa.Text(), nullable=True),
        sa.Column("is_breakpoint", sa.Boolean(), nullable=False),
        sa.Column("breakpoint_type", sqlmodel.sql.sqltypes.AutoString(), nullable=True),
        sa.Column("duration_ms", sa.Integer(), nullable=True),
        sa.ForeignKeyConstraint(
            ["conversation_id"],
            ["conversations.id"],
        ),
        sa.ForeignKeyConstraint(
            ["run_id"],
            ["agent_runs.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("approval_audits", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_approval_audits_conversation_id"),
            ["conversation_id"],
            unique=False,
        )
        batch_op.create_index(
            batch_op.f("ix_approval_audits_run_id"),
            ["run_id"],
            unique=False,
        )

    # Add capability_policy column to conversations (Фаза 1.5 §2).
    with op.batch_alter_table("conversations", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column("capability_policy", sa.JSON(), nullable=True)
        )


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("conversations", schema=None) as batch_op:
        batch_op.drop_column("capability_policy")

    with op.batch_alter_table("approval_audits", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_approval_audits_run_id"))
        batch_op.drop_index(batch_op.f("ix_approval_audits_conversation_id"))

    op.drop_table("approval_audits")
