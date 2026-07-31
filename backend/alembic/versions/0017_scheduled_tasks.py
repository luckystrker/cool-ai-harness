"""Add scheduled_tasks and task_runs (Фаза 3b §1, §2, §3).

Revision ID: 0017
Revises: 0016
"""

import sqlalchemy as sa

from alembic import op

revision = "0017"
down_revision = "0016"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "scheduled_tasks",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("name", sa.String(), nullable=False),
        sa.Column("description", sa.Text(), nullable=True),
        sa.Column("trigger_type", sa.String(), nullable=False),
        sa.Column("cron_expression", sa.String(), nullable=True),
        sa.Column("interval_seconds", sa.Integer(), nullable=True),
        sa.Column("run_at", sa.DateTime(), nullable=True),
        sa.Column("timezone", sa.String(), nullable=False),
        sa.Column("quiet_hours_start", sa.String(), nullable=True),
        sa.Column("quiet_hours_end", sa.String(), nullable=True),
        sa.Column("misfire_policy", sa.String(), nullable=False),
        sa.Column("prompt", sa.Text(), nullable=False),
        sa.Column("workflow_type", sa.String(), nullable=True),
        sa.Column("profile_id", sa.Integer(), nullable=True),
        sa.Column("model", sa.String(), nullable=True),
        sa.Column("tools_whitelist", sa.JSON(), nullable=True),
        sa.Column("capability_policy", sa.JSON(), nullable=True),
        sa.Column("working_directory", sa.String(), nullable=True),
        sa.Column("approval_policy", sa.String(), nullable=False),
        sa.Column("delivery_channels", sa.JSON(), nullable=True),
        sa.Column("delivery_config", sa.JSON(), nullable=True),
        sa.Column("last_delivery_hash", sa.String(), nullable=True),
        sa.Column("max_iterations", sa.Integer(), nullable=False),
        sa.Column("max_cost_per_run", sa.Float(), nullable=True),
        sa.Column("timeout_s", sa.Float(), nullable=True),
        sa.Column("enabled", sa.Boolean(), nullable=False),
        sa.Column("next_run_at", sa.DateTime(), nullable=True),
        sa.Column("last_run_at", sa.DateTime(), nullable=True),
        sa.Column("last_status", sa.String(), nullable=True),
        sa.Column("run_count", sa.Integer(), nullable=False),
        sa.Column("failure_count", sa.Integer(), nullable=False),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.ForeignKeyConstraint(["profile_id"], ["agent_profiles.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_scheduled_tasks_user_id", "scheduled_tasks", ["user_id"])
    op.create_index("ix_scheduled_tasks_name", "scheduled_tasks", ["name"])
    op.create_index("ix_scheduled_tasks_trigger_type", "scheduled_tasks", ["trigger_type"])
    op.create_index("ix_scheduled_tasks_profile_id", "scheduled_tasks", ["profile_id"])
    op.create_index("ix_scheduled_tasks_enabled", "scheduled_tasks", ["enabled"])
    op.create_index("ix_scheduled_tasks_next_run_at", "scheduled_tasks", ["next_run_at"])

    op.create_table(
        "task_runs",
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("task_id", sa.Integer(), nullable=False),
        sa.Column("conversation_id", sa.Integer(), nullable=True),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("status", sa.String(), nullable=False),
        sa.Column("trigger_source", sa.String(), nullable=False),
        sa.Column("prompt", sa.Text(), nullable=False),
        sa.Column("output", sa.Text(), nullable=True),
        sa.Column("error", sa.Text(), nullable=True),
        sa.Column("skip_reason", sa.String(), nullable=True),
        sa.Column("approval_policy", sa.String(), nullable=True),
        sa.Column("approval_reason", sa.Text(), nullable=True),
        sa.Column("usage", sa.JSON(), nullable=True),
        sa.Column("duration_ms", sa.Integer(), nullable=True),
        sa.Column("delivery_status", sa.JSON(), nullable=True),
        sa.Column("delivered_at", sa.DateTime(), nullable=True),
        sa.Column("is_read", sa.Boolean(), nullable=False),
        sa.Column("started_at", sa.DateTime(), nullable=False),
        sa.Column("finished_at", sa.DateTime(), nullable=True),
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["task_id"], ["scheduled_tasks.id"]),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
        sa.ForeignKeyConstraint(["run_id"], ["agent_runs.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    op.create_index("ix_task_runs_task_id", "task_runs", ["task_id"])
    op.create_index("ix_task_runs_status", "task_runs", ["status"])
    op.create_index("ix_task_runs_is_read", "task_runs", ["is_read"])


def downgrade() -> None:
    op.drop_index("ix_task_runs_is_read", table_name="task_runs")
    op.drop_index("ix_task_runs_status", table_name="task_runs")
    op.drop_index("ix_task_runs_task_id", table_name="task_runs")
    op.drop_table("task_runs")
    op.drop_index("ix_scheduled_tasks_next_run_at", table_name="scheduled_tasks")
    op.drop_index("ix_scheduled_tasks_enabled", table_name="scheduled_tasks")
    op.drop_index("ix_scheduled_tasks_profile_id", table_name="scheduled_tasks")
    op.drop_index("ix_scheduled_tasks_trigger_type", table_name="scheduled_tasks")
    op.drop_index("ix_scheduled_tasks_name", table_name="scheduled_tasks")
    op.drop_index("ix_scheduled_tasks_user_id", table_name="scheduled_tasks")
    op.drop_table("scheduled_tasks")
