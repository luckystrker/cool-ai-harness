"""Alembic environment.

Sources the database URL and target metadata from the application itself so a
single source of truth (``app.core.config`` settings + ``app.models``) drives
both runtime schema creation and migrations.

SQLite is the default dev database; ``render_as_batch=True`` makes ALTER
operations work there (SQLite has limited ALTER support, so Alembic emulates
them via table copy).
"""

from __future__ import annotations

import sys
from logging.config import fileConfig
from pathlib import Path

from sqlalchemy import engine_from_config, pool
from sqlmodel import SQLModel

from alembic import context

# Ensure the backend package is importable (alembic.ini lives in backend/).
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

# Importing the models package registers every SQLModel table on
# SQLModel.metadata, which autogenerate then diffs against the live database.
from app import models  # noqa: F401
from app.core.config import get_settings

# this is the Alembic Config object, which provides
# access to the values within the .ini file in use.
config = context.config

# Interpret the config file for Python logging.
if config.config_file_name is not None:
    fileConfig(config.config_file_name)

# Single source of truth for the schema: SQLModel.metadata (all table=True
# models register here on import of app.models).
target_metadata = SQLModel.metadata

# The DB URL comes from the app settings (env-driven), not the .ini file.
settings = get_settings()
config.set_main_option("sqlalchemy.url", settings.database_url)


def run_migrations_offline() -> None:
    """Run migrations in 'offline' mode (emit SQL without a live connection)."""
    url = config.get_main_option("sqlalchemy.url")
    context.configure(
        url=url,
        target_metadata=target_metadata,
        literal_binds=True,
        dialect_opts={"paramstyle": "named"},
        render_as_batch=True,
    )

    with context.begin_transaction():
        context.run_migrations()


def run_migrations_online() -> None:
    """Run migrations against a live database connection."""
    connectable = engine_from_config(
        config.get_section(config.config_ini_section, {}),
        prefix="sqlalchemy.",
        poolclass=pool.NullPool,
    )

    with connectable.connect() as connection:
        is_sqlite = connection.dialect.name == "sqlite"
        context.configure(
            connection=connection,
            target_metadata=target_metadata,
            # Batch mode emulates ALTER for SQLite (and is a no-op elsewhere).
            render_as_batch=is_sqlite,
        )

        with context.begin_transaction():
            context.run_migrations()


if context.is_offline_mode():
    run_migrations_offline()
else:
    run_migrations_online()
