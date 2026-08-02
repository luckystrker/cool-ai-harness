"""Add rss_subscriptions and rss_entries (Фаза 3b §6).

Revision ID: 0019
Revises: 0018
"""

import sqlalchemy as sa

from alembic import op

revision = "0019"
down_revision = "0018"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "rss_subscriptions",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("url", sa.String(), nullable=False),
        sa.Column("title", sa.String(), nullable=True),
        sa.Column("site_url", sa.String(), nullable=True),
        sa.Column("category", sa.String(), nullable=True),
        sa.Column("fetch_interval_minutes", sa.Integer(), nullable=False),
        sa.Column("enabled", sa.Boolean(), nullable=False),
        sa.Column("last_fetched_at", sa.DateTime(), nullable=True),
        sa.Column("last_error", sa.Text(), nullable=True),
        sa.Column("entry_count", sa.Integer(), nullable=False),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_rss_subscriptions_user_id", "rss_subscriptions", ["user_id"])
    op.create_index("ix_rss_subscriptions_url", "rss_subscriptions", ["url"])
    op.create_index("ix_rss_subscriptions_category", "rss_subscriptions", ["category"])
    op.create_index("ix_rss_subscriptions_enabled", "rss_subscriptions", ["enabled"])

    op.create_table(
        "rss_entries",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("subscription_id", sa.Integer(), nullable=False),
        sa.Column("guid", sa.String(), nullable=False),
        sa.Column("title", sa.String(), nullable=True),
        sa.Column("link", sa.String(), nullable=True),
        sa.Column("author", sa.String(), nullable=True),
        sa.Column("summary", sa.Text(), nullable=True),
        sa.Column("published_at", sa.DateTime(), nullable=True),
        sa.Column("content_hash", sa.String(), nullable=True),
        sa.Column("is_read", sa.Boolean(), nullable=False),
        sa.Column("fetched_at", sa.DateTime(), nullable=False),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["subscription_id"], ["rss_subscriptions.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_rss_entries_subscription_id", "rss_entries", ["subscription_id"])
    op.create_index("ix_rss_entries_guid", "rss_entries", ["guid"])
    op.create_index("ix_rss_entries_published_at", "rss_entries", ["published_at"])
    op.create_index("ix_rss_entries_content_hash", "rss_entries", ["content_hash"])
    op.create_index("ix_rss_entries_is_read", "rss_entries", ["is_read"])


def downgrade() -> None:
    op.drop_index("ix_rss_entries_is_read", table_name="rss_entries")
    op.drop_index("ix_rss_entries_content_hash", table_name="rss_entries")
    op.drop_index("ix_rss_entries_published_at", table_name="rss_entries")
    op.drop_index("ix_rss_entries_guid", table_name="rss_entries")
    op.drop_index("ix_rss_entries_subscription_id", table_name="rss_entries")
    op.drop_table("rss_entries")
    op.drop_index("ix_rss_subscriptions_enabled", table_name="rss_subscriptions")
    op.drop_index("ix_rss_subscriptions_category", table_name="rss_subscriptions")
    op.drop_index("ix_rss_subscriptions_url", table_name="rss_subscriptions")
    op.drop_index("ix_rss_subscriptions_user_id", table_name="rss_subscriptions")
    op.drop_table("rss_subscriptions")
