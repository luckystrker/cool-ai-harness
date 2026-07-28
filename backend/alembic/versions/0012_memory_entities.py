"""memory_items.pinned + entities/entity_relations/memory_item_entities (Фаза 3a — Memory)

Revision ID: 0012
Revises: 0011
Create Date: 2026-07-28 10:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0012"
down_revision: str | Sequence[str] | None = "0011"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    # --- memory_items.pinned (user protection from decay/TTL) ---
    op.add_column(
        "memory_items",
        sa.Column("pinned", sa.Boolean(), nullable=False, server_default=sa.text("0")),
    )
    op.create_index("ix_memory_items_pinned", "memory_items", ["pinned"])

    # --- entities (named entity memory) ---
    op.create_table(
        "entities",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("name", sa.String(), nullable=False),
        sa.Column("entity_type", sa.String(), nullable=False, server_default="concept"),
        sa.Column("aliases", sa.JSON(), nullable=True),
        sa.Column("attributes", sa.JSON(), nullable=True),
        sa.Column("description", sa.Text(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.UniqueConstraint("user_id", "name", name="uq_entities_user_id_name"),
    )
    op.create_index("ix_entities_user_id", "entities", ["user_id"])
    op.create_index("ix_entities_name", "entities", ["name"])
    op.create_index("ix_entities_entity_type", "entities", ["entity_type"])

    # --- entity_relations (directed relationships between entities) ---
    op.create_table(
        "entity_relations",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("source_entity_id", sa.Integer(), nullable=False),
        sa.Column("target_entity_id", sa.Integer(), nullable=False),
        sa.Column("relation_type", sa.String(), nullable=False, server_default="related_to"),
        sa.Column("attributes", sa.JSON(), nullable=True),
        sa.PrimaryKeyConstraint("id"),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.ForeignKeyConstraint(["source_entity_id"], ["entities.id"]),
        sa.ForeignKeyConstraint(["target_entity_id"], ["entities.id"]),
    )
    op.create_index("ix_entity_relations_user_id", "entity_relations", ["user_id"])
    op.create_index("ix_entity_relations_source_entity_id", "entity_relations", ["source_entity_id"])
    op.create_index("ix_entity_relations_target_entity_id", "entity_relations", ["target_entity_id"])

    # --- memory_item_entities (link table: memory <-> entity) ---
    op.create_table(
        "memory_item_entities",
        sa.Column("memory_id", sa.Integer(), nullable=False),
        sa.Column("entity_id", sa.Integer(), nullable=False),
        sa.PrimaryKeyConstraint("memory_id", "entity_id"),
        sa.ForeignKeyConstraint(["memory_id"], ["memory_items.id"]),
        sa.ForeignKeyConstraint(["entity_id"], ["entities.id"]),
    )


def downgrade() -> None:
    """Downgrade schema."""
    op.drop_table("memory_item_entities")
    op.drop_index("ix_entity_relations_target_entity_id", table_name="entity_relations")
    op.drop_index("ix_entity_relations_source_entity_id", table_name="entity_relations")
    op.drop_index("ix_entity_relations_user_id", table_name="entity_relations")
    op.drop_table("entity_relations")
    op.drop_index("ix_entities_entity_type", table_name="entities")
    op.drop_index("ix_entities_name", table_name="entities")
    op.drop_index("ix_entities_user_id", table_name="entities")
    op.drop_table("entities")
    op.drop_index("ix_memory_items_pinned", table_name="memory_items")
    op.drop_column("memory_items", "pinned")
