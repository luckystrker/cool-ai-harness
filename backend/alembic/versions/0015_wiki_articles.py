"""Add wiki_articles table (Knowledge Base / Wiki — Фаза 3a §3).

Revision ID: 0015
Revises: 0014
"""

import sqlalchemy as sa

from alembic import op

revision = "0015"
down_revision = "0014"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "wiki_articles",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("title", sa.String(), nullable=False),
        sa.Column("content", sa.Text(), nullable=False, server_default=""),
        sa.Column("category", sa.String(), nullable=False, server_default="general"),
        sa.Column("tags", sa.JSON(), nullable=True),
        sa.Column("source", sa.String(), nullable=False, server_default="manual"),
        sa.Column("source_memory_id", sa.Integer(), nullable=True),
        sa.Column("user_id", sa.Integer(), nullable=True),
        sa.Column("project_key", sa.String(), nullable=True),
        sa.Column("is_pinned", sa.Boolean(), nullable=False, server_default="0"),
        sa.Column("is_archived", sa.Boolean(), nullable=False, server_default="0"),
        sa.Column("version", sa.Integer(), nullable=False, server_default="1"),
        sa.Column("metadata_", sa.JSON(), nullable=True),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
    )
    op.create_index("ix_wiki_articles_title", "wiki_articles", ["title"])
    op.create_index("ix_wiki_articles_category", "wiki_articles", ["category"])
    op.create_index("ix_wiki_articles_user_id", "wiki_articles", ["user_id"])
    op.create_index("ix_wiki_articles_project_key", "wiki_articles", ["project_key"])
    op.create_index("ix_wiki_articles_is_archived", "wiki_articles", ["is_archived"])


def downgrade() -> None:
    op.drop_index("ix_wiki_articles_is_archived", table_name="wiki_articles")
    op.drop_index("ix_wiki_articles_project_key", table_name="wiki_articles")
    op.drop_index("ix_wiki_articles_user_id", table_name="wiki_articles")
    op.drop_index("ix_wiki_articles_category", table_name="wiki_articles")
    op.drop_index("ix_wiki_articles_title", table_name="wiki_articles")
    op.drop_table("wiki_articles")
