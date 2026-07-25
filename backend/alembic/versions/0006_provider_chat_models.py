"""providers.chat_models — the subset of a provider's models offered in chat

Revision ID: 0006
Revises: 0005
Create Date: 2026-07-25 11:00:00.000000

Adds ``providers.chat_models``: a JSON list of model ids the user has marked
as available in the chat model picker (selected from the provider's live
``/models`` list in the provider settings). Empty/NULL means "no models
exposed in chat yet".

The pre-existing ``default_model`` column is kept as-is for compatibility but
is no longer surfaced in the settings UI; the first entry of ``chat_models``
is used as the effective default when none is named per-conversation.
"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0006"
down_revision: str | Sequence[str] | None = "0005"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("providers", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column("chat_models", sa.JSON(), nullable=True)
        )


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("providers", schema=None) as batch_op:
        batch_op.drop_column("providers", "chat_models")
