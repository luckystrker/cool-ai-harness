"""Database engine and session management (SQLModel/SQLAlchemy)."""

from __future__ import annotations

from collections.abc import Generator

from sqlalchemy import inspect, text
from sqlmodel import Session, SQLModel, create_engine

from app.core.config import get_settings

_settings = get_settings()

# check_same_thread=False: FastAPI may use threads; we rely on session-per-request.
connect_args = {"check_same_thread": False} if _settings.database_url.startswith("sqlite") else {}

engine = create_engine(
    _settings.database_url,
    echo=False,
    connect_args=connect_args,
)


# Columns added after the initial schema. Each entry is (table, column, DDL).
# Alembic migrations remain the production path; this lightweight auto-migrate
# just keeps an existing dev DB working when a new nullable column ships, so a
# fresh `data/harness.db` isn't required on every schema change.
_LIGHTWEIGHT_MIGRATIONS: list[tuple[str, str, str]] = [
    ("messages", "thinking", "TEXT"),
    ("conversations", "working_directory", "TEXT"),
    ("conversations", "permissions", "JSON"),
]


def init_db() -> None:
    """Create all tables. Called on startup.

    In production, apply Alembic migrations (the source of truth for the live
    schema). In development / tests, fall back to ``create_all`` + the
    lightweight auto-migrate: models are the source of truth there, and this
    avoids the overhead of the migration runner on every test-bootstrapped DB.
    """
    # Import models so SQLModel.metadata sees them before create_all.
    from app import models  # noqa: F401

    settings = get_settings()
    if settings.environment == "production":
        _run_alembic_upgrade()
    else:
        SQLModel.metadata.create_all(engine)
        _apply_lightweight_migrations()


def _run_alembic_upgrade() -> None:
    """Apply Alembic migrations up to head (production schema path).

    Uses the same alembic.ini the CLI uses (``backend/alembic.ini``), so a
    single set of migrations drives both manual and startup-driven upgrades.
    Errors are loud: a failed migration should stop startup, not silently run
    against a half-migrated schema.
    """
    from pathlib import Path

    from alembic.config import Config

    from alembic import command

    backend_root = Path(__file__).resolve().parents[2]
    cfg = Config(str(backend_root / "alembic.ini"))
    # The DB URL is resolved inside env.py from app settings; we only need to
    # point alembic at the versions directory it already knows about.
    cfg.set_main_option("script_location", str(backend_root / "alembic"))
    command.upgrade(cfg, "head")


def _apply_lightweight_migrations() -> None:
    """Add any not-yet-present columns from _LIGHTWEIGHT_MIGRATIONS.

    Safe and idempotent: existing columns are left untouched. Only nullable
    columns (no NOT NULL, no default required) belong here.
    """
    insp = inspect(engine)
    with Session(engine) as session:
        for table, column, ddl_type in _LIGHTWEIGHT_MIGRATIONS:
            if not insp.has_table(table):
                continue
            existing = {col["name"] for col in insp.get_columns(table)}
            if column in existing:
                continue
            session.execute(text(f'ALTER TABLE "{table}" ADD COLUMN "{column}" {ddl_type}'))
        session.commit()
    # Refresh the inspector cache so later introspection in the same process
    # sees the new columns.
    insp = inspect(engine)


def get_session() -> Generator[Session, None, None]:
    """FastAPI dependency: yield a per-request DB session."""
    with Session(engine) as session:
        yield session
