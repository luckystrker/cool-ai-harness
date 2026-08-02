"""Add composite indexes for hot query paths (performance).

Addresses linear-scan degradation on append-only tables: run_events,
tool_calls, memory_items, episodes, spend_log.

Revision ID: 0018
Revises: 0017
"""

from alembic import op

revision = "0018"
down_revision = "0017"
branch_labels = None
depends_on = None


def upgrade() -> None:
    # run_events: timeline queries filter by run_id + kind, ordered by created_at.
    op.create_index("ix_run_events_run_id_kind", "run_events", ["run_id", "kind"])
    op.create_index("ix_run_events_created_at", "run_events", ["created_at"])

    # tool_calls: analytics filter by created_at + success.
    op.create_index("ix_tool_calls_created_at_success", "tool_calls", ["created_at", "success"])

    # memory_items: recall filters by conversation_id + created_at, user + status.
    op.create_index(
        "ix_memory_items_conversation_created",
        "memory_items",
        ["conversation_id", "created_at"],
    )
    op.create_index("ix_memory_items_user_status", "memory_items", ["user_id", "status"])

    # episodes: scoped by conversation.
    op.create_index("ix_episodes_conversation_id", "episodes", ["conversation_id"])

    # spend_log: budget queries aggregate by user + period.
    op.create_index("ix_spend_log_user_created", "spend_log", ["user_id", "created_at"])

    # task_runs: unread count polls by is_read + created_at.
    op.create_index("ix_task_runs_is_read_created", "task_runs", ["is_read", "created_at"])


def downgrade() -> None:
    op.drop_index("ix_task_runs_is_read_created", table_name="task_runs")
    op.drop_index("ix_spend_log_user_created", table_name="spend_log")
    op.drop_index("ix_episodes_conversation_id", table_name="episodes")
    op.drop_index("ix_memory_items_user_status", table_name="memory_items")
    op.drop_index("ix_memory_items_conversation_created", table_name="memory_items")
    op.drop_index("ix_tool_calls_created_at_success", table_name="tool_calls")
    op.drop_index("ix_run_events_created_at", table_name="run_events")
    op.drop_index("ix_run_events_run_id_kind", table_name="run_events")
