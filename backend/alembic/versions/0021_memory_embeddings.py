"""memory_embeddings metadata + memory_vec vec0 virtual table (Фаза 3a §1 hybrid index)

Revision ID: 0021
Revises: 4869d4c0322a
Create Date: 2026-08-08 12:00:00.000000

The vector itself lives in the sqlite-vec ``vec0`` virtual table; the regular
``memory_embeddings`` table tracks the producing model/dimension for the
backfill sweep. Requires the sqlite-vec extension at runtime (retrieval
degrades gracefully to FTS5 when it's unavailable).
"""

import logging
from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0021"
down_revision: str | Sequence[str] | None = "4869d4c0322a"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.create_table(
        "memory_embeddings",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("memory_id", sa.Integer(), nullable=False),
        sa.Column("model", sa.String(), nullable=False, server_default=""),
        sa.Column("dimension", sa.Integer(), nullable=False, server_default="1536"),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["memory_id"], ["memory_items.id"]),
    )
    op.create_index(
        "ix_memory_embeddings_memory_id", "memory_embeddings", ["memory_id"], unique=True
    )

    # vec0 virtual table (sqlite-vec): memory_id as the primary key rowid,
    # cosine distance metric. The DDL must match the runtime expectation in
    # app/memory/embeddings.py. Optional: without the sqlite-vec extension
    # this fails and retrieval degrades gracefully to FTS5-only.
    try:
        op.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vec USING vec0("
            "memory_id INTEGER PRIMARY KEY, "
            "embedding FLOAT[1536] distance_metric=cosine"
            ")"
        )
    except Exception as exc:
        logging.getLogger("alembic.runtime.migration").warning(
            "memory_vec not created (sqlite-vec unavailable): %s", exc
        )


def downgrade() -> None:
    """Downgrade schema."""
    op.execute("DROP TABLE IF EXISTS memory_vec")
    op.drop_index("ix_memory_embeddings_memory_id", table_name="memory_embeddings")
    op.drop_table("memory_embeddings")
