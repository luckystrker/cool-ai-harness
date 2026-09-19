"""M10 store-contract tests between the Rust legacy store and the Python schema.

Three guarantees are checked here:

1. the committed baseline snapshot still matches the real Alembic chain;
2. the Python runtime refuses to run `create_all`/Alembic against a store the
   Rust runtime has adopted (no competing migration owners);
3. rows written by either runtime are readable by the other, including
   SQLAlchemy's `DateTime` parsing of Rust-written timestamps.

The cross-language tests need a Rust toolchain and are exercised by the
`store-parity` CI job; they skip only when `cargo` is unavailable.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
from sqlalchemy import create_engine, text

BACKEND_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = BACKEND_ROOT.parent
SNAPSHOT = REPO_ROOT / "crates" / "cool-store" / "tests" / "fixtures" / "python_schema_0022.sql"

cargo_required = pytest.mark.skipif(
    shutil.which("cargo") is None,
    reason="cargo toolchain required for cross-language store parity",
)


def _database_url(path: Path) -> str:
    return f"sqlite:///{path.as_posix()}"


def _upgrade_to_head(path: Path) -> None:
    environment = os.environ.copy()
    environment["DATABASE_URL"] = _database_url(path)
    result = subprocess.run(
        [sys.executable, "-m", "alembic", "upgrade", "head"],
        cwd=BACKEND_ROOT,
        env=environment,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"


def _run_fixture_tool(command: str, path: Path) -> str:
    result = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "cool-store",
            "--example",
            "fixture_tool",
            "--",
            command,
            str(path),
        ],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"
    return result.stdout.strip()


def test_schema_snapshot_matches_the_alembic_chain() -> None:
    result = subprocess.run(
        [sys.executable, "-m", "tests.schema_snapshot", "--check", str(SNAPSHOT)],
        cwd=BACKEND_ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"


def test_python_refuses_to_migrate_a_rust_owned_store(tmp_path, monkeypatch) -> None:
    from app.core import db as core_db

    path = tmp_path / "rust-owned.db"
    engine = create_engine(_database_url(path))
    with engine.begin() as connection:
        connection.execute(
            text("CREATE TABLE rust_store_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        )
        connection.execute(
            text("INSERT INTO rust_store_meta(key, value) VALUES ('owner', 'rust')")
        )

    monkeypatch.setattr(core_db, "engine", engine)
    with pytest.raises(RuntimeError, match="Rust store"):
        core_db.init_db()

    # A store without the marker is not blocked: the guard keys on ownership,
    # not on the presence of the table.
    with engine.begin() as connection:
        connection.execute(text("DELETE FROM rust_store_meta"))
    core_db.init_db()


def test_alembic_cli_refuses_a_rust_owned_store(tmp_path) -> None:
    path = tmp_path / "rust-owned-alembic.db"
    _upgrade_to_head(path)
    engine = create_engine(_database_url(path))
    with engine.begin() as connection:
        connection.execute(
            text("CREATE TABLE rust_store_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        )
        connection.execute(
            text("INSERT INTO rust_store_meta(key, value) VALUES ('owner', 'rust')")
        )

    environment = os.environ.copy()
    environment["DATABASE_URL"] = _database_url(path)
    result = subprocess.run(
        [sys.executable, "-m", "alembic", "upgrade", "head"],
        cwd=BACKEND_ROOT,
        env=environment,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0, "alembic CLI must fail closed on a Rust-owned store"
    assert "Rust store" in f"{result.stdout}\n{result.stderr}"


@cargo_required
def test_python_reads_rows_written_by_the_rust_store(tmp_path) -> None:
    path = tmp_path / "harness.db"
    _upgrade_to_head(path)
    _run_fixture_tool("write", path)

    engine = create_engine(_database_url(path))
    with engine.begin() as connection:
        revision = connection.execute(text("SELECT version_num FROM alembic_version")).scalar_one()
        owner = connection.execute(
            text("SELECT value FROM rust_store_meta WHERE key = 'owner'")
        ).scalar_one()
        title = connection.execute(text("SELECT title FROM conversations")).scalar_one()
        content = connection.execute(text("SELECT content FROM messages")).scalar_one()
        status, started_at = connection.execute(
            text("SELECT status, started_at FROM agent_runs")
        ).one()
        kind, payload = connection.execute(text("SELECT kind, payload FROM run_events")).one()

    assert revision == "0022", "the Rust adoption must not change alembic_version"
    assert owner == "rust"
    assert title == "Rust parity smoke"
    assert content == "written by rust"
    assert status == "completed"
    assert kind == "run.started"
    assert json.loads(payload)["provider"] == "rust"
    assert isinstance(started_at, str) and " " in started_at

    # SQLAlchemy must parse the Rust-written datetimes through the ORM exactly
    # like rows written by Python (format compatibility, not just string shape).
    from sqlmodel import Session, select

    from app.models import AgentRun

    with Session(engine) as session:
        run = session.exec(select(AgentRun)).one()
        assert run.started_at is not None
        assert run.finished_at is not None
        assert run.finished_at >= run.started_at


@cargo_required
def test_rust_reads_rows_written_by_python(tmp_path) -> None:
    path = tmp_path / "harness.db"
    _upgrade_to_head(path)
    engine = create_engine(_database_url(path))
    with engine.begin() as connection:
        connection.execute(
            text(
                "INSERT INTO users(created_at, updated_at, id, external_id, username, "
                "display_name, is_active) VALUES ('2026-01-01 00:00:00.000000', "
                "'2026-01-01 00:00:00.000000', 1, 'local', 'local', 'Local', 1)"
            )
        )
        connection.execute(
            text(
                "INSERT INTO conversations(created_at, updated_at, id, user_id, title, "
                "is_pinned, is_archived) VALUES ('2026-01-01 00:00:00.000000', "
                "'2026-01-01 00:00:00.000000', 1, 1, 'Python chat', 0, 0)"
            )
        )
        connection.execute(
            text(
                "INSERT INTO messages(created_at, updated_at, id, conversation_id, role, "
                "content) VALUES ('2026-01-01 00:00:01.000000', '2026-01-01 00:00:01.000000', "
                "1, 1, 'user', 'written by python')"
            )
        )
        connection.execute(
            text(
                "INSERT INTO agent_runs(created_at, updated_at, id, conversation_id, user_id, "
                "status, model, iterations, started_at, finished_at) VALUES "
                "('2026-01-01 00:00:02.000000', '2026-01-01 00:00:03.000000', 1, 1, 1, "
                "'completed', 'python-model', 2, '2026-01-01 00:00:02.000000', "
                "'2026-01-01 00:00:03.000000')"
            )
        )
        connection.execute(
            text(
                "INSERT INTO run_events(created_at, updated_at, id, run_id, seq, kind, payload) "
                "VALUES ('2026-01-01 00:00:02.000000', '2026-01-01 00:00:02.000000', 1, 1, 1, "
                "'run.started', '{\"provider\": \"python\"}')"
            )
        )

    output = json.loads(_run_fixture_tool("read", path))
    assert len(output) == 1
    conversation = output[0]
    assert conversation["title"] == "Python chat"
    assert conversation["model"] is None
    assert conversation["messages"] == [{"role": "user", "content": "written by python"}]
    assert conversation["runs"][0]["status"] == "completed"
    assert conversation["runs"][0]["model"] == "python-model"
    assert conversation["runs"][0]["usage"] is None
    assert conversation["runs"][0]["events"] == [
        {"seq": 1, "kind": "run.started", "payload": {"provider": "python"}}
    ]
