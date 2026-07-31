"""Add delegate_role to plan_steps (subplan delegation).

Revision ID: 0014
Revises: 0013
"""

import sqlalchemy as sa

from alembic import op

revision = "0014"
down_revision = "0013"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("plan_steps", sa.Column("delegate_role", sa.String(), nullable=True))


def downgrade() -> None:
    op.drop_column("plan_steps", "delegate_role")
