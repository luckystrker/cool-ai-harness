"""providers.is_default — explicit default provider flag

Revision ID: 0007
Revises: 0006
Create Date: 2026-07-25 12:00:00.000000

Adds ``providers.is_default``: marks the one provider that is the primary
backend for new conversations. When set, that provider's first chat-exposed
model (``chat_models[0]``) is used as the default model for a newly created
conversation, and the provider is the primary in the resilience chain.

The flag is mutually exclusive at most one row per user (the API enforces
this on update/create). When no row has the flag, the pre-existing rule
(first active, non-fallback row) still applies, so this is additive.
"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "0007"
down_revision: str | Sequence[str] | None = "0006"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    """Upgrade schema."""
    with op.batch_alter_table("providers", schema=None) as batch_op:
        batch_op.add_column(
            sa.Column("is_default", sa.Boolean(), nullable=False, server_default=sa.text("0"))
        )
        batch_op.create_index(batch_op.f("ix_providers_is_default"), ["is_default"], unique=False)


def downgrade() -> None:
    """Downgrade schema."""
    with op.batch_alter_table("providers", schema=None) as batch_op:
        batch_op.drop_index(batch_op.f("ix_providers_is_default"))
        batch_op.drop_column("providers", "is_default")
