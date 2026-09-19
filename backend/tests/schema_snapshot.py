"""Dump the production Alembic schema as a SQL snapshot for Rust store tests.

The Rust store (``crates/cool-store``) adopts the existing Python SQLite schema
at a fixed baseline revision.  This script materialises that baseline from the
real Alembic migration chain so the committed snapshot cannot silently drift
from the migrations.

Usage (from ``backend/``)::

    python -m tests.schema_snapshot --update <path/to/python_schema_0022.sql>
    python -m tests.schema_snapshot --check <path/to/python_schema_0022.sql>

``--update`` writes three committed artifacts:

* the SQL snapshot (used verbatim by the Rust fixture loader),
* a sibling ``.sha256`` digest that the Rust tests check,
* a sibling ``.fingerprint.json`` with a deterministic *semantic* schema
  fingerprint (tables, columns, foreign keys, indexes, triggers, virtual
  tables).

The fingerprint exists because SQLAlchemy rebuilds batch-migrated SQLite
tables through reflection, whose constraint/index ordering varies run to run.
``--check`` therefore regenerates the fingerprint from the real migration chain
and compares it with the committed document instead of byte-comparing DDL.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

BACKEND_ROOT = Path(__file__).resolve().parents[1]

# The Rust store supports exactly this Alembic revision as its baseline.
BASELINE_REVISION = "0022"

# sqlite-vec is a loadable extension; the committed snapshot must be loadable
# without it, so the vec0 table (and only it) is excluded.
EXCLUDED_TABLE_PREFIXES = ("sqlite_", "memory_vec")
# FTS5 shadow tables are rebuilt by SQLite when the virtual table is created.
FTS_SHADOW_TABLES = {
    "memory_fts_data",
    "memory_fts_idx",
    "memory_fts_content",
    "memory_fts_docsize",
    "memory_fts_config",
}

HEADER = (
    "-- Baseline schema snapshot for the Rust legacy store (crates/cool-store).\n"
    "-- Generated from the real Alembic migration chain; do not edit by hand.\n"
    "-- Regenerate: backend $ python -m tests.schema_snapshot --update <this file>\n"
    f"-- Alembic revision: {BASELINE_REVISION}\n"
    "-- Excludes the sqlite-vec `memory_vec` table and FTS5 shadow tables:\n"
    "-- SQLite rebuilds the FTS shadow tables from the virtual table DDL, and\n"
    "-- the Rust store must keep working on databases without sqlite-vec.\n"
)


def _database_url(path: Path) -> str:
    return f"sqlite:///{path.as_posix()}"


def _upgrade_to_head(database_url: str) -> None:
    environment = os.environ.copy()
    environment["DATABASE_URL"] = database_url
    result = subprocess.run(
        [sys.executable, "-m", "alembic", "upgrade", "head"],
        cwd=str(BACKEND_ROOT),
        env=environment,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"alembic upgrade failed ({result.returncode}):\n{result.stdout}\n{result.stderr}"
        )


def _included(name: str) -> bool:
    if name in FTS_SHADOW_TABLES:
        return False
    return not name.startswith(EXCLUDED_TABLE_PREFIXES)


def _normalize_sql(sql: str) -> str:
    return " ".join(sql.split())


def _table_fingerprint(connection: sqlite3.Connection, table: str) -> dict[str, Any]:
    quoted = table.replace('"', '""')
    columns = [
        {
            "name": row[1],
            "type": (row[2] or "").upper(),
            "notnull": row[3],
            "default": row[4],
            "pk": row[5],
        }
        for row in connection.execute(f'PRAGMA table_info("{quoted}")')
    ]
    foreign_keys = sorted(
        (
            {
                "table": row[2],
                "from": row[3],
                "to": row[4],
                "on_update": row[5],
                "on_delete": row[6],
                "match": row[7],
            }
            for row in connection.execute(f'PRAGMA foreign_key_list("{quoted}")')
        ),
        key=lambda item: (item["table"], item["from"], item["to"]),
    )
    indexes = []
    for index_row in connection.execute(f'PRAGMA index_list("{quoted}")'):
        index_name = index_row[1]
        index_quoted = index_name.replace('"', '""')
        index_columns = [
            column_row[2]
            for column_row in connection.execute(f'PRAGMA index_info("{index_quoted}")')
        ]
        indexes.append(
            {
                "name": index_name,
                "unique": bool(index_row[2]),
                "origin": index_row[3],
                "columns": index_columns,
            }
        )
    indexes.sort(key=lambda item: item["name"])
    return {"columns": columns, "foreign_keys": foreign_keys, "indexes": indexes}


def build_fingerprint(connection: sqlite3.Connection) -> dict[str, Any]:
    revision = connection.execute("SELECT version_num FROM alembic_version").fetchone()
    if revision is None:
        raise RuntimeError("alembic_version has no row after upgrade")
    rows = connection.execute(
        "SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name"
    ).fetchall()
    tables: dict[str, Any] = {}
    triggers: dict[str, str] = {}
    virtual_tables: dict[str, str] = {}
    for object_type, name, sql in rows:
        if not _included(name):
            continue
        if object_type == "trigger":
            triggers[name] = _normalize_sql(sql)
        elif object_type == "table" and sql.lstrip().upper().startswith("CREATE VIRTUAL TABLE"):
            virtual_tables[name] = _normalize_sql(sql)
        elif object_type == "table":
            tables[name] = _table_fingerprint(connection, name)
    return {
        "alembic_revision": revision[0],
        "tables": dict(sorted(tables.items())),
        "triggers": dict(sorted(triggers.items())),
        "virtual_tables": dict(sorted(virtual_tables.items())),
    }


def _build_snapshot_rows(connection: sqlite3.Connection) -> list[tuple[str, str, str]]:
    return connection.execute(
        "SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid"
    ).fetchall()


def build_snapshot() -> tuple[str, dict[str, Any]]:
    """Create a temporary migrated database and return (SQL snapshot, fingerprint)."""
    with tempfile.TemporaryDirectory(prefix="cool-schema-") as temporary:
        database_path = Path(temporary) / "harness.db"
        _upgrade_to_head(_database_url(database_path))
        connection = sqlite3.connect(database_path)
        try:
            rows = _build_snapshot_rows(connection)
            fingerprint = build_fingerprint(connection)
        finally:
            connection.close()

    statements = []
    for object_type, name, sql in rows:
        if not _included(name):
            continue
        del object_type
        statement = sql.strip().rstrip(";")
        statements.append(f"{statement};")
    statements.append(f"INSERT INTO alembic_version(version_num) VALUES ('{BASELINE_REVISION}');")
    return HEADER + "\n".join(statements) + "\n", fingerprint


def _digest(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def _fingerprint_path(snapshot: Path) -> Path:
    return snapshot.with_suffix(snapshot.suffix + ".fingerprint.json")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--update", type=Path, help="write the snapshot to this path")
    group.add_argument("--check", type=Path, help="verify the snapshot at this path")
    arguments = parser.parse_args(argv)

    snapshot, fingerprint = build_snapshot()
    content = snapshot.encode("utf-8")
    fingerprint_text = json.dumps(fingerprint, indent=2, sort_keys=True) + "\n"
    if arguments.update is not None:
        target = arguments.update.resolve()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        target.with_suffix(target.suffix + ".sha256").write_text(_digest(content) + "\n")
        _fingerprint_path(target).write_text(fingerprint_text, encoding="utf-8")
        print(f"wrote {target} ({len(content)} bytes)")
        return 0

    expected = arguments.check.resolve()
    if not expected.exists():
        print(f"missing snapshot: {expected}", file=sys.stderr)
        return 2
    committed_digest = expected.with_suffix(expected.suffix + ".sha256")
    if not committed_digest.exists() or committed_digest.read_text().strip() != _digest(
        expected.read_bytes()
    ):
        print(f"schema snapshot digest mismatch: {committed_digest}", file=sys.stderr)
        return 1
    committed_fingerprint = _fingerprint_path(expected)
    if not committed_fingerprint.exists():
        print(f"missing fingerprint: {committed_fingerprint}", file=sys.stderr)
        return 2
    if committed_fingerprint.read_text(encoding="utf-8") != fingerprint_text:
        print(
            f"schema fingerprint drift: {expected} does not match the Alembic chain",
            file=sys.stderr,
        )
        return 1
    print(f"schema snapshot ok ({len(content)} bytes, sha256 {_digest(content)[:12]}...)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
