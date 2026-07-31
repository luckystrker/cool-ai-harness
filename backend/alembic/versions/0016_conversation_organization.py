"""Add conversation organization fields (Фаза 3a §4).

Revision ID: 0016
Revises: 0015
"""

import sqlalchemy as sa

from alembic import op

revision = "0016"
down_revision = "0015"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("conversations", sa.Column("tags", sa.JSON(), nullable=True))
    op.add_column("conversations", sa.Column("folder", sa.String(), nullable=True))
    op.add_column(
        "conversations",
        sa.Column("is_pinned", sa.Boolean(), nullable=False, server_default="0"),
    )
    op.add_column(
        "conversations",
        sa.Column("is_archived", sa.Boolean(), nullable=False, server_default="0"),
    )
    op.create_index("ix_conversations_folder", "conversations", ["folder"])
    op.create_index("ix_conversations_is_pinned", "conversations", ["is_pinned"])
    op.create_index("ix_conversations_is_archived", "conversations", ["is_archived"])


def downgrade() -> None:
    op.drop_index("ix_conversations_is_archived", table_name="conversations")
    op.drop_index("ix_conversations_is_pinned", table_name="conversations")
    op.drop_index("ix_conversations_folder", table_name="conversations")
    op.drop_column("conversations", "is_archived")
    op.drop_column("conversations", "is_pinned")
    op.drop_column("conversations", "folder")
    op.drop_column("conversations", "tags")
