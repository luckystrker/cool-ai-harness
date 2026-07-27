"""memory_items, episodes, working_memory tables + FTS5 index (Фаза 3a — Memory)

Revision ID: 0011
Revises: 0010
Create Date: 2026-07-27 12:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0011"
down_revision: str | Sequence[str] | None = "0010"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    # --- memory_items ---
    op.create_table(
        "memory_items",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        # Namespace / Scope
        sa.Column("scope", sa.String(), nullable=False, server_default="global"),
        sa.Column("agent_id", sa.Integer(), nullable=True),
        sa.Column("conversation_id", sa.Integer(), nullable=True),
        # Content
        sa.Column("memory_type", sa.String(), nullable=False, server_default="semantic"),
        sa.Column("content", sa.Text(), nullable=False),
        sa.Column("structured", sa.JSON(), nullable=True),
        sa.Column("tags", sa.JSON(), nullable=True),
        # Metadata
        sa.Column("importance", sa.Float(), nullable=False, server_default="0.5"),
        sa.Column("confidence", sa.Float(), nullable=False, server_default="0.7"),
        sa.Column("source", sa.String(), nullable=False, server_default="agent"),
        sa.Column("status", sa.String(), nullable=False, server_default="active"),
        sa.Column("supersedes_id", sa.Integer(), nullable=True),
        # Lifecycle
        sa.Column("access_count", sa.Integer(), nullable=False, server_default="0"),
        sa.Column("last_accessed_at", sa.DateTime(), nullable=True),
        sa.Column("ttl_days", sa.Integer(), nullable=True),
        sa.Column("valid_from", sa.DateTime(), nullable=True),
        sa.Column("valid_to", sa.DateTime(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
    )
    op.create_index("ix_memory_items_user_id", "memory_items", ["user_id"])
    op.create_index("ix_memory_items_scope", "memory_items", ["scope"])
    op.create_index("ix_memory_items_agent_id", "memory_items", ["agent_id"])
    op.create_index("ix_memory_items_memory_type", "memory_items", ["memory_type"])
    op.create_index("ix_memory_items_status", "memory_items", ["status"])

    # --- episodes ---
    op.create_table(
        "episodes",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("agent_id", sa.Integer(), nullable=True),
        sa.Column("conversation_id", sa.Integer(), nullable=True),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("title", sa.String(), nullable=False),
        sa.Column("summary", sa.Text(), nullable=False),
        sa.Column("outcome", sa.String(), nullable=False, server_default="unknown"),
        sa.Column("importance", sa.Float(), nullable=False, server_default="0.5"),
        sa.Column("tags", sa.JSON(), nullable=True),
        sa.Column("related_entities", sa.JSON(), nullable=True),
        sa.Column("started_at", sa.DateTime(), nullable=True),
        sa.Column("ended_at", sa.DateTime(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
        sa.ForeignKeyConstraint(["run_id"], ["agent_runs.id"]),
    )
    op.create_index("ix_episodes_user_id", "episodes", ["user_id"])
    op.create_index("ix_episodes_agent_id", "episodes", ["agent_id"])

    # --- working_memory ---
    op.create_table(
        "working_memory",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("conversation_id", sa.Integer(), nullable=False),
        sa.Column("state", sa.JSON(), nullable=False, server_default="{}"),
        sa.Column("summary", sa.Text(), nullable=True),
        sa.Column("summary_up_to_message_id", sa.Integer(), nullable=True),
        sa.Column("token_estimate", sa.Integer(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
        sa.UniqueConstraint("conversation_id"),
    )
    op.create_index("ix_working_memory_conversation_id", "working_memory", ["conversation_id"])

    # --- FTS5 virtual table for full-text search over memory_items ---
    # Uses content-sync mode: the FTS index mirrors memory_items.content and tags.
    op.execute(
        """
        CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
            content,
            tags,
            memory_type UNINDEXED,
            content='memory_items',
            content_rowid='id'
        )
        """
    )

    # Triggers to keep FTS5 in sync with memory_items.
    op.execute(
        """
        CREATE TRIGGER IF NOT EXISTS memory_items_ai AFTER INSERT ON memory_items BEGIN
            INSERT INTO memory_fts(rowid, content, tags, memory_type)
            VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);
        END
        """
    )
    op.execute(
        """
        CREATE TRIGGER IF NOT EXISTS memory_items_ad AFTER DELETE ON memory_items BEGIN
            INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)
            VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);
        END
        """
    )
    op.execute(
        """
        CREATE TRIGGER IF NOT EXISTS memory_items_au AFTER UPDATE ON memory_items BEGIN
            INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)
            VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);
            INSERT INTO memory_fts(rowid, content, tags, memory_type)
            VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);
        END
        """
    )


def downgrade() -> None:
    """Downgrade schema."""
    op.execute("DROP TRIGGER IF EXISTS memory_items_au")
    op.execute("DROP TRIGGER IF EXISTS memory_items_ad")
    op.execute("DROP TRIGGER IF EXISTS memory_items_ai")
    op.execute("DROP TABLE IF EXISTS memory_fts")
    op.drop_table("working_memory")
    op.drop_table("episodes")
    op.drop_table("memory_items")
