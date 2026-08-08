"""Database engine and session management (SQLModel/SQLAlchemy)."""

from __future__ import annotations

from collections.abc import Generator

from sqlalchemy import event, inspect, text
from sqlmodel import Session, SQLModel, create_engine

from app.core.config import get_settings
from app.core.logging import get_logger

_settings = get_settings()

log = get_logger(__name__)

# check_same_thread=False: FastAPI may use threads; we rely on session-per-request.
connect_args = {"check_same_thread": False} if _settings.database_url.startswith("sqlite") else {}

engine = create_engine(
    _settings.database_url,
    echo=False,
    connect_args=connect_args,
)

# SQLite performance pragmas: WAL mode allows concurrent readers during writes,
# synchronous=NORMAL avoids an fsync per commit (huge throughput gain), and
# busy_timeout prevents immediate SQLITE_BUSY errors under contention.
if _settings.database_url.startswith("sqlite"):

    @event.listens_for(engine, "connect")
    def _set_sqlite_pragmas(dbapi_conn, connection_record):
        cursor = dbapi_conn.cursor()
        cursor.execute("PRAGMA journal_mode=WAL")
        cursor.execute("PRAGMA synchronous=NORMAL")
        cursor.execute("PRAGMA busy_timeout=5000")
        cursor.close()


# --- sqlite-vec (vector search over memory embeddings) ---
# Loaded per-connection (SQLite loadable extensions are per-connection). The
# extension may be unavailable (Python builds without loadable-extension
# support); memory retrieval degrades gracefully to FTS5 in that case.
VEC_AVAILABLE = False

if _settings.database_url.startswith("sqlite"):
    try:
        import sqlite_vec

        @event.listens_for(engine, "connect")
        def _load_vec_extension(dbapi_conn, connection_record):
            try:
                dbapi_conn.enable_load_extension(True)
                sqlite_vec.load(dbapi_conn)
                dbapi_conn.enable_load_extension(False)
            except Exception:
                # Not fatal: vector search falls back to FTS5-only retrieval.
                pass

        VEC_AVAILABLE = True
    except Exception:
        VEC_AVAILABLE = False


# Columns added after the initial schema. Each entry is (table, column, DDL).
# Alembic migrations remain the production path; this lightweight auto-migrate
# just keeps an existing dev DB working when a new nullable column ships, so a
# fresh `data/harness.db` isn't required on every schema change.
_LIGHTWEIGHT_MIGRATIONS: list[tuple[str, str, str]] = [
    ("messages", "thinking", "TEXT"),
    ("conversations", "working_directory", "TEXT"),
    ("conversations", "permissions", "JSON"),
    ("conversations", "capability_policy", "JSON"),
    ("conversations", "metadata_", "JSON"),
    # Фаза 1.5 §5 — provider fallback flag (new tables budgets/spend_log are
    # created by create_all; only the added column on an existing table needs
    # an explicit ALTER here).
    ("providers", "is_fallback", "BOOLEAN DEFAULT 0"),
    # 0006 — JSON list of model ids exposed in the chat picker.
    ("providers", "chat_models", "JSON"),
    # 0007 — explicit default-provider flag.
    ("providers", "is_default", "BOOLEAN DEFAULT 0"),
    # 0008 — per-message model + turn duration.
    ("messages", "model", "TEXT"),
    ("messages", "duration_ms", "INTEGER"),
    # 0013 — agent profiles (Фаза 3a §2).
    ("conversations", "profile_id", "INTEGER"),
    ("subagent_runs", "profile_id", "INTEGER"),
]


# Memory virtual tables created by migrations 0011 (FTS5 mirror) and 0021
# (vec0 vector table) — SQLModel.create_all doesn't know about them, so the
# dev/test path recreates them here. DDL mirrors the migrations verbatim.
_FTS_MIRROR_DDL = (
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5("
    "content, tags, memory_type UNINDEXED, content='memory_items', content_rowid='id')"
)
_FTS_MIRROR_TRIGGERS = [
    (
        "CREATE TRIGGER IF NOT EXISTS memory_items_ai AFTER INSERT ON memory_items BEGIN"
        " INSERT INTO memory_fts(rowid, content, tags, memory_type)"
        " VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);"
        " END"
    ),
    (
        "CREATE TRIGGER IF NOT EXISTS memory_items_ad AFTER DELETE ON memory_items BEGIN"
        " INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)"
        " VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);"
        " END"
    ),
    (
        "CREATE TRIGGER IF NOT EXISTS memory_items_au AFTER UPDATE ON memory_items BEGIN"
        " INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)"
        " VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);"
        " INSERT INTO memory_fts(rowid, content, tags, memory_type)"
        " VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);"
        " END"
    ),
]
_VEC_TABLE_DDL = (
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vec USING vec0("
    "memory_id INTEGER PRIMARY KEY, embedding FLOAT[1536] distance_metric=cosine)"
)


def _create_memory_virtual_tables() -> None:
    """Create the FTS5 mirror + vec0 table that create_all doesn't cover.

    Idempotent (IF NOT EXISTS everywhere). The vec0 table needs the sqlite-vec
    extension; when it's unavailable the DDL fails and retrieval degrades to
    FTS5-only — logged and non-fatal, with the FTS mirror already committed.
    """
    with Session(engine) as session:
        session.execute(text(_FTS_MIRROR_DDL))
        for trigger in _FTS_MIRROR_TRIGGERS:
            session.execute(text(trigger))
        session.commit()
        try:
            session.execute(text(_VEC_TABLE_DDL))
            session.commit()
        except Exception as exc:
            session.rollback()
            log.warning("db.memory_vec_unavailable", error=str(exc))


def init_db() -> None:
    """Create all tables. Called on startup.

    In production, apply Alembic migrations (the source of truth for the live
    schema). In development / tests, fall back to ``create_all`` + the
    lightweight auto-migrate: models are the source of truth there, and this
    avoids the overhead of the migration runner on every test-bootstrapped DB.
    """
    # Import models so SQLModel.metadata sees them before create_all.
    from app import models  # noqa: F401

    # Memory models are imported separately to avoid circular imports.
    from app.memory import models as memory_models  # noqa: F401

    settings = get_settings()
    if settings.environment == "production":
        _run_alembic_upgrade()
    else:
        SQLModel.metadata.create_all(engine)
        _apply_lightweight_migrations()
        _create_memory_virtual_tables()
        _hide_legacy_test_conversations()
    _seed_profiles()


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


# Conversation titles the backend test suite historically used when it created
# rows directly in the real database (before the isolated test DB existed).
# Their high repeat counts make them unambiguous test artifacts; a real user
# doesn't end up with dozens of identically-named chats. Flagging them (rather
# than deleting) keeps the data intact while removing the clutter from the UI.
_LEGACY_TEST_CONVERSATION_TITLES = (
    "art test",
    "WS",
    "ws",
    "WS-err",
    "Renamed",
    "tool-rt",
    "gated",
    "thinking",
    "log",
    "approval",
    "plain",
    "link",
    "fail",
    "r",
    "capability-test",
    "audit-test",
    "audit-denied",
    "audit-allowed",
    "tool-test",
    "UI smoke",
    "Smoke test",
    "My chat",
)


def _hide_legacy_test_conversations() -> None:
    """Flag conversations left behind by old (pre-isolation) test runs.

    Sets ``metadata_.is_test`` so the conversations list endpoint hides them.
    Idempotent and non-destructive: already-flagged rows are skipped and
    nothing is deleted.
    """
    from sqlmodel import select

    from app.models import Conversation

    with Session(engine) as session:
        rows = session.exec(
            select(Conversation).where(
                Conversation.title.in_(_LEGACY_TEST_CONVERSATION_TITLES)  # type: ignore[union-attr]
            )
        ).all()
        changed = False
        for row in rows:
            meta = dict(row.metadata_ or {})
            if meta.get("is_test"):
                continue
            meta["is_test"] = True
            row.metadata_ = meta
            session.add(row)
            changed = True
        if changed:
            session.commit()


def get_session() -> Generator[Session, None, None]:
    """FastAPI dependency: yield a per-request DB session."""
    with Session(engine) as session:
        yield session


def _seed_profiles() -> None:
    """Seed built-in agent profiles (Фаза 3a §2). Idempotent."""
    from app.agent.personalities.seeding import seed_builtin_profiles

    with Session(engine) as session:
        seed_builtin_profiles(session)
