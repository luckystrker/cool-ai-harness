"""Database engine and session management (SQLModel/SQLAlchemy)."""

from __future__ import annotations

from collections.abc import Generator

from sqlmodel import Session, SQLModel, create_engine

from app.core.config import get_settings

_settings = get_settings()

# check_same_thread=False: FastAPI may use threads; we rely on session-per-request.
connect_args = (
    {"check_same_thread": False} if _settings.database_url.startswith("sqlite") else {}
)

engine = create_engine(
    _settings.database_url,
    echo=False,
    connect_args=connect_args,
)


def init_db() -> None:
    """Create all tables. Called on startup.

    In production, prefer Alembic migrations; this is convenient for MVP/dev.
    """
    # Import models so SQLModel.metadata sees them before create_all.
    from app import models  # noqa: F401

    SQLModel.metadata.create_all(engine)


def get_session() -> Generator[Session, None, None]:
    """FastAPI dependency: yield a per-request DB session."""
    with Session(engine) as session:
        yield session
