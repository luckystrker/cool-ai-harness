"""Add webhook_endpoints and webhook_events (Фаза 3b §7).

Revision ID: 0020
Revises: 0019
"""

import sqlalchemy as sa

from alembic import op

revision = "0020"
down_revision = "0019"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "webhook_endpoints",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("name", sa.String(), nullable=False),
        sa.Column("hook_id", sa.String(), nullable=False),
        sa.Column("secret", sa.String(), nullable=False),
        sa.Column("source_type", sa.String(), nullable=False),
        sa.Column("event_filter", sa.JSON(), nullable=True),
        sa.Column("task_id", sa.Integer(), nullable=True),
        sa.Column("prompt_template", sa.Text(), nullable=True),
        sa.Column("enabled", sa.Boolean(), nullable=False),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.ForeignKeyConstraint(["task_id"], ["scheduled_tasks.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_webhook_endpoints_user_id", "webhook_endpoints", ["user_id"])
    op.create_index("ix_webhook_endpoints_name", "webhook_endpoints", ["name"])
    op.create_index("ix_webhook_endpoints_hook_id", "webhook_endpoints", ["hook_id"], unique=True)
    op.create_index("ix_webhook_endpoints_source_type", "webhook_endpoints", ["source_type"])
    op.create_index("ix_webhook_endpoints_enabled", "webhook_endpoints", ["enabled"])

    op.create_table(
        "webhook_events",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("endpoint_id", sa.Integer(), nullable=False),
        sa.Column("event_type", sa.String(), nullable=True),
        sa.Column("payload", sa.JSON(), nullable=True),
        sa.Column("signature_valid", sa.Boolean(), nullable=False),
        sa.Column("status", sa.String(), nullable=False),
        sa.Column("task_run_id", sa.Integer(), nullable=True),
        sa.Column("error", sa.Text(), nullable=True),
        sa.Column("received_at", sa.DateTime(), nullable=False),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["endpoint_id"], ["webhook_endpoints.id"]),
        sa.ForeignKeyConstraint(["task_run_id"], ["task_runs.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_webhook_events_endpoint_id", "webhook_events", ["endpoint_id"])
    op.create_index("ix_webhook_events_event_type", "webhook_events", ["event_type"])
    op.create_index("ix_webhook_events_status", "webhook_events", ["status"])


def downgrade() -> None:
    op.drop_index("ix_webhook_events_status", table_name="webhook_events")
    op.drop_index("ix_webhook_events_event_type", table_name="webhook_events")
    op.drop_index("ix_webhook_events_endpoint_id", table_name="webhook_events")
    op.drop_table("webhook_events")
    op.drop_index("ix_webhook_endpoints_enabled", table_name="webhook_endpoints")
    op.drop_index("ix_webhook_endpoints_source_type", table_name="webhook_endpoints")
    op.drop_index("ix_webhook_endpoints_hook_id", table_name="webhook_endpoints")
    op.drop_index("ix_webhook_endpoints_name", table_name="webhook_endpoints")
    op.drop_index("ix_webhook_endpoints_user_id", table_name="webhook_endpoints")
    op.drop_table("webhook_endpoints")
