"""messages.model + messages.duration_ms — per-message model and turn duration

Revision ID: 0008
Revises: 0007
Create Date: 2026-07-25 13:00:00.000000

Adds two nullable columns to ``messages`` so each assistant message records
which model produced it and how long the whole turn (agent loop) took:

  * ``model`` (TEXT)  — the model id snapshot at turn time (also on
    AgentRun, but duplicated here so history renders without a join).
  * ``duration_ms`` (INTEGER) — wall-clock milliseconds from the start of the
    turn to the finish event. Whole-turn granularity (a turn may span several
    loop iterations with tool calls).

Both default to NULL; existing rows simply show nothing extra. The runner
fills them on the finish event, alongside the existing usage attachment.
"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0008"
down_revision: str | Sequence[str] | None = "0007"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("messages", schema=None) as batch_op:
        batch_op.add_column(sa.Column("model", sa.Text(), nullable=True))
        batch_op.add_column(sa.Column("duration_ms", sa.Integer(), nullable=True))


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("messages", schema=None) as batch_op:
        batch_op.drop_column("messages", "duration_ms")
        batch_op.drop_column("messages", "model")
