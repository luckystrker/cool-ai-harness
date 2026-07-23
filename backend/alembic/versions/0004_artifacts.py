"""artifacts table (Фаза 1.5 §3 — artifacts & attachments)

Revision ID: 0004
Revises: 0003
Create Date: 2026-07-24 10:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa
import sqlmodel

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0004"
down_revision: str | Sequence[str] | None = "0003"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.create_table(
        "artifacts",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("conversation_id", sa.Integer(), nullable=False),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("tool_call_id", sqlmodel.sql.sqltypes.AutoString(), nullable=True),
        sa.Column("filename", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("media_type", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("kind", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("size_bytes", sa.Integer(), nullable=False),
        sa.Column("sha256", sqlmodel.sql.sqltypes.AutoString(), nullable=True),
        sa.Column("storage_path", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("version", sa.Integer(), nullable=False),
        sa.Column("parent_id", sa.Integer(), nullable=True),
        sa.Column("metadata_", sa.JSON(), nullable=True),
        sa.Column("extracted_text", sa.Text(), nullable=True),
        sa.Column("is_deleted", sa.Boolean(), nullable=False),
        sa.ForeignKeyConstraint(
            ["conversation_id"],
            ["conversations.id"],
        ),
        sa.ForeignKeyConstraint(
            ["run_id"],
            ["agent_runs.id"],
        ),
        sa.ForeignKeyConstraint(
            ["parent_id"],
            ["artifacts.id"],
        ),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("artifacts", schema=None) as batch_op:
        batch_op.create_index(
            batch_op.f("ix_artifacts_conversation_id"),
            ["conversation_id"],
            unique=False,
        )
        batch_op.create_index(
            batch_op.f("ix_artifacts_run_id"),
            ["run_id"],
            unique=False,
        )
        batch_op.create_index(
            batch_op.f("ix_artifacts_kind"),
            ["kind"],
            unique=False,
        )
        batch_op.create_index(
            batch_op.f("ix_artifacts_sha256"),
            ["sha256"],
            unique=False,
        )
        batch_op.create_index(
            batch_op.f("ix_artifacts_is_deleted"),
            ["is_deleted"],
            unique=False,
        )


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("artifacts", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_artifacts_is_deleted"))
        batch_op.drop_index(batch_op.f("ix_artifacts_sha256"))
        batch_op.drop_index(batch_op.f("ix_artifacts_kind"))
        batch_op.drop_index(batch_op.f("ix_artifacts_run_id"))
        batch_op.drop_index(batch_op.f("ix_artifacts_conversation_id"))

    op.drop_table("artifacts")
