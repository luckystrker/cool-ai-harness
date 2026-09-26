# ADR-0003: Removal of the legacy Python server

- Status: **Proposed** — execution requires maintainer approval and is gated
  by the plan's release rule (§13: no legacy-code removal in the first
  default-cutover commit; §14.13: removal no earlier than two stable
  Rust-default releases).
- Date: 2026-09-26
- Scope: `backend/app` FastAPI server, its startup path, and the Python-only
  test/eval surface that guards it.

## Context

As of M12 the Rust trusted core is the default runtime: `cool serve` owns the
SPA, the canonical App Protocol (`POST /api/rpc`), the cursor/reconnect event
stream (`GET /api/events`), the blob transports, and every React-facing
subsystem. The production image contains no Python. The legacy
`backend/app` FastAPI server remains in the tree as the reference
implementation and as the optional-lane codebase (evals, future optional
Python workers), but it is no longer on any startup path.

Keeping the legacy server indefinitely has real costs:

- **Dual-surface drift.** Every protocol/store change must keep the Python
  models, routers and event shapes in sync even though no supported install
  runs them. The shared `frontend/src/api/types.ts` contract and the
  Alembic/`SQLModel.create_all` split are maintained for a runtime that ships
  nothing.
- **Security surface.** `backend/` carries the sandbox, SSRF, capability and
  secrets code. Unused-but-present server code still gets dependency updates
  and review attention.
- **Ownership confusion.** New contributors see two servers and cannot tell
  which is authoritative.

## Decision

Delete the legacy Python **server** (`backend/app/api`, `backend/app/main.py`,
the FastAPI/Uvicorn startup path and everything that exists only to serve it)
in a dedicated release that contains no other behavioral change, after:

1. M12 has shipped and the Rust default has run for **two consecutive
   releases** without a runtime rollback (plan §M12 exit criterion).
2. This ADR is marked **Accepted** by the maintainer.

What is deleted vs. kept is enumerated below so the removal commit is a
mechanical change, not a design decision.

### Deleted with the server

- `backend/app/api/` routers, `main.py`, websocket/SSE plumbing, and any
  helper used only by the HTTP surface.
- The Python agent runtime **if and only if** each capability it owns has a
  Rust equivalent or an explicit optional-worker disposition recorded in
  `docs/backlog/python-workers.md`. The M12 parity matrix lives in
  `docs/migration/checkpoints/M12.md` — every row must be `rust` or
  `optional-worker` before deletion starts.
- `backend/alembic/` migrations and the `SQLModel` schema (the Rust store owns
  the schema at baseline `0022`; Alembic has no authority over a Rust-owned
  database).
- Python-only dependencies in `pyproject.toml` that exist solely for the
  server.

### Kept (moved, not deleted)

- `backend/evals/` — the deterministic eval scenarios are a product-quality
  gate. Before the server is deleted they are re-pointed at the Rust runtime
  (scripted-driver equivalents already exist in `crates/cool-agent/tests/`),
  or moved to a standalone `evals/` package that exercises the canonical
  protocol. The gate must not silently disappear.
- The optional-Python-worker contract in `docs/backlog/python-workers.md`
  stays as the only sanctioned way Python code runs against the core:
  out-of-process, capability-scoped, no host secrets.
- Historical artifacts: `docs/migration/checkpoints/*`, golden fixtures, and
  the committed schema snapshots keep referring to the Python baseline — they
  are migration evidence, not live code.

### Out of scope

- Deleting `sdk/typescript` (it is the canonical client, not a compatibility
  shim), the `cool-extensions` worker protocol, or the OpenCode Bun worker.
- The Telegram adapter (parked in `docs/backlog/telegram-adapter.md`).
- Data migration: Rust-owned stores are already the only writable form; this
  ADR changes no schema.

## Rollback

The removal commit is a pure deletion — rollback is `git revert` of that
commit, which restores the code but not any capability removed from the
checkout's expectations. Because the deletion release changes nothing else,
reverting it is safe at any time. Data-level rollback is covered by the M12
rollback release document (`docs/migration/M12_ROLLBACK.md`): a Python-owned
pre-adoption backup can always be served by the last Python-containing
release.

## Consequences

- Positive: one server, one schema owner, one security model; CI drops the
  `backend` job once evals are re-pointed; `frontend/src/api/types.ts` and the
  Python schema mirrors are deleted with it.
- Negative: the reference implementation is gone — bugs can no longer be
  answered by reading the Python side. Mitigation: this ADR only proceeds once
  the parity matrix in `M12.md` is complete and two Rust-default releases have
  shipped without rollback.
- `AGENTS.md` and `README.md` are updated in the same commit that deletes the
  server, removing the `backend/app` sections and the Python dev commands.
