"""budgets, spend_log, and providers.is_fallback (Фаза 1.5 §5 — cost guards)

Revision ID: 0005
Revises: 0004
Create Date: 2026-07-25 10:00:00.000000

Adds:
  - ``budgets``: per-user cost-budget config (daily/weekly/monthly limits,
    alert threshold, block behavior, override window).
  - ``spend_log``: append-only per-LLM-call cost rows for spend history.
  - ``providers.is_fallback``: marks a provider as the backup model used when
    the primary is unhealthy (retry/circuit-breaker fallback chain).
"""

from collections.abc import Sequence

import sqlalchemy as sa
import sqlmodel

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0005"
down_revision: str | Sequence[str] | None = "0004"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    op.create_table(
        "budgets",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("daily_limit_usd", sa.Float(), nullable=True),
        sa.Column("weekly_limit_usd", sa.Float(), nullable=True),
        sa.Column("monthly_limit_usd", sa.Float(), nullable=True),
        sa.Column("alert_threshold_pct", sa.Float(), nullable=False),
        sa.Column("block_on_exceed", sa.Boolean(), nullable=False),
        sa.Column("override_until", sa.DateTime(), nullable=True),
        sa.Column("last_alert_at", sa.DateTime(), nullable=True),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("budgets", schema=None) as batch_op:
        batch_op.create_index(batch_op.f("ix_budgets_user_id"), ["user_id"], unique=False)

    op.create_table(
        "spend_log",
        sa.Column("created_at", sa.DateTime(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
        sa.Column("id", sa.Integer(), nullable=False),
        sa.Column("user_id", sa.Integer(), nullable=False),
        sa.Column("run_id", sa.Integer(), nullable=True),
        sa.Column("conversation_id", sa.Integer(), nullable=True),
        sa.Column("provider_name", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("model", sqlmodel.sql.sqltypes.AutoString(), nullable=False),
        sa.Column("prompt_tokens", sa.Integer(), nullable=False),
        sa.Column("completion_tokens", sa.Integer(), nullable=False),
        sa.Column("total_tokens", sa.Integer(), nullable=False),
        sa.Column("cost_usd", sa.Float(), nullable=False),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.ForeignKeyConstraint(["user_id"], ["users.id"]),
        sa.ForeignKeyConstraint(["run_id"], ["agent_runs.id"]),
        sa.ForeignKeyConstraint(["conversation_id"], ["conversations.id"]),
        sa.PrimaryKeyConstraint("id"),
    )
    with op.batch_alter_table("spend_log", schema=None) as batch_op:
        batch_op.create_index(batch_op.f("ix_spend_log_user_id"), ["user_id"], unique=False)
        batch_op.create_index(batch_op.f("ix_spend_log_run_id"), ["run_id"], unique=False)
        batch_op.create_index(
            batch_op.f("ix_spend_log_conversation_id"), ["conversation_id"], unique=False
        )
        batch_op.create_index(batch_op.f("ix_spend_log_ts"), ["ts"], unique=False)
        # Composite index for windowed "spend since X for user Y" queries.
        batch_op.create_index(
            "ix_spend_log_user_ts", ["user_id", "ts"], unique=False
        )

    with op.batch_alter_table("providers", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column("is_fallback", sa.Boolean(), nullable=False, server_default=sa.text("0"))
        )
        batch_op.create_index(batch_op.f("ix_providers_is_fallback"), ["is_fallback"], unique=False)


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("providers", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_providers_is_fallback"))
        batch_op.drop_column("providers", "is_fallback")

    with op.batch_alter_table("spend_log", schema=None) as batch_op:
        batch_op.drop_index("ix_spend_log_user_ts")
        batch_op.drop_index(batch_op.f("ix_spend_log_ts"))
        batch_op.drop_index(batch_op.f("ix_spend_log_conversation_id"))
        batch_op.drop_index(batch_op.f("ix_spend_log_run_id"))
        batch_op.drop_index(batch_op.f("ix_spend_log_user_id"))

    op.drop_table("spend_log")

    with op.batch_alter_table("budgets", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_budgets_user_id"))

    op.drop_table("budgets")
