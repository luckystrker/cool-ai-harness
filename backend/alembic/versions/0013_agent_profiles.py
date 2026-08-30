"""agent_profiles table + conversations.profile_id (Фаза 3a §2 — Multi-personality agents)

Revision ID: 0013
Revises: 0012
Create Date: 2026-07-29 10:00:00.000000

"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0013"
down_revision: str | Sequence[str] | None = "0012"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    # --- agent_profiles ---
    op.create_table(
        "agent_profiles",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("name", sa.String(), nullable=False),
        sa.Column("slug", sa.String(), nullable=False),
        sa.Column("description", sa.Text(), nullable=True),
        sa.Column("system_prompt", sa.Text(), nullable=True),
        sa.Column("model", sa.String(), nullable=True),
        sa.Column("tool_names", sa.JSON(), nullable=True),
        sa.Column("skill_names", sa.JSON(), nullable=True),
        sa.Column("settings", sa.JSON(), nullable=True),
        sa.Column("avatar_color", sa.String(), nullable=True),
        sa.Column("is_builtin", sa.Boolean(), nullable=False, server_default=sa.text("0")),
        sa.Column("is_active", sa.Boolean(), nullable=False, server_default=sa.text("1")),
        sa.PrimaryKeyConstraint("id"),
        sa.UniqueConstraint("slug", name="uq_agent_profiles_slug"),
    )
    op.create_index("ix_agent_profiles_name", "agent_profiles", ["name"])
    op.create_index("ix_agent_profiles_slug", "agent_profiles", ["slug"])
    op.create_index("ix_agent_profiles_is_active", "agent_profiles", ["is_active"])

    # --- conversations.profile_id FK ---
    # SQLite cannot add a foreign-key constraint with ALTER TABLE. Batch mode
    # rebuilds the table and also works on PostgreSQL/MySQL.
    with op.batch_alter_table("conversations") as batch_op:
        batch_op.add_column(sa.Column("profile_id", sa.Integer(), nullable=True))
        batch_op.create_foreign_key(
            "fk_conversations_profile_id",
            "agent_profiles",
            ["profile_id"],
            ["id"],
        )
        batch_op.create_index("ix_conversations_profile_id", ["profile_id"])

    # --- subagent_runs.profile_id FK (cross-profile invocation) ---
    with op.batch_alter_table("subagent_runs") as batch_op:
        batch_op.add_column(sa.Column("profile_id", sa.Integer(), nullable=True))
        batch_op.create_foreign_key(
            "fk_subagent_runs_profile_id",
            "agent_profiles",
            ["profile_id"],
            ["id"],
        )
        batch_op.create_index("ix_subagent_runs_profile_id", ["profile_id"])


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("subagent_runs") as batch_op:
        batch_op.drop_index("ix_subagent_runs_profile_id")
        batch_op.drop_constraint("fk_subagent_runs_profile_id", type_="foreignkey")
        batch_op.drop_column("profile_id")
    with op.batch_alter_table("conversations") as batch_op:
        batch_op.drop_index("ix_conversations_profile_id")
        batch_op.drop_constraint("fk_conversations_profile_id", type_="foreignkey")
        batch_op.drop_column("profile_id")
    op.drop_index("ix_agent_profiles_is_active", table_name="agent_profiles")
    op.drop_index("ix_agent_profiles_slug", table_name="agent_profiles")
    op.drop_index("ix_agent_profiles_name", table_name="agent_profiles")
    op.drop_table("agent_profiles")
