# Cool — migration to the Rust core

> **Status: complete.** M0–M12 are all done. M12 closed the migration: the
> Rust runtime is the only runtime and the legacy Python server, its test/eval
> lane and the M0 spike were removed under
> [ADR-0003](migration/adr/0003-remove-legacy-python-server.md) (accepted and
> executed by maintainer directive, 2026-09-27).
>
> **This file is an archive.** It used to carry the full working plan
> (per-milestone specs, exit criteria, gate procedure) in Russian; the executed
> record now lives in [`docs/migration/`](migration/) — the
> [checkpoints](migration/checkpoints/) M0–M12 hold the per-milestone evidence
> and residual lists, and the [ADRs](migration/adr/) record the binding
> decisions. The original plan text is in git history.
>
> **Product roadmap:** the active coding-agent roadmap is
> [`docs/PLAN.md`](PLAN.md).

## What the migration delivered

Target architecture: a Rust trusted core + versioned App Protocol + React web
UI + Rust TUI + ACP adapter + protocol-isolated extensions — replacing the
Python (FastAPI) runtime incrementally rather than as a big-bang rewrite.

| # | Milestone | Status | Evidence |
|---:|---|---|---|
| 0 | M0 — Architecture ADR and vertical spike | ✅ complete | [checkpoint](migration/checkpoints/M0.md) |
| 1 | M1 — Rust protocol foundation and golden corpus | ✅ complete | [checkpoint](migration/checkpoints/M1.md) |
| 2 | M2 — Packaging/entrypoint contract | ✅ complete | [checkpoint](migration/checkpoints/M2.md) |
| 3 | M3 — Standard plugin contract | ✅ complete | [checkpoint](migration/checkpoints/M3.md) |
| 4 | M4 — ACP adapter; real-client acceptance collected post-M11 | ✅ complete | [checkpoint](migration/checkpoints/M4.md) |
| 5 | M5 — App server and CLI skeleton | ✅ complete | [checkpoint](migration/checkpoints/M5.md) |
| 6 | M6 — Durable state and security kernel | ✅ complete | [checkpoint](migration/checkpoints/M6.md) |
| 7 | M7 — Agent loop and trusted tool runtime | ✅ complete | [checkpoint](migration/checkpoints/M7.md) |
| 8 | M8 — MCP, plugins, hooks and workers | ✅ complete | [checkpoint](migration/checkpoints/M8.md) |
| 9 | M9 — Rust CLI/TUI and ACP cutover | ✅ complete | [checkpoint](migration/checkpoints/M9.md) |
| 10 | M10 — Store and background subsystems parity | ✅ complete | [checkpoint](migration/checkpoints/M10.md) |
| 11 | M11 — Web cutover and compatibility workers (reduced recorded scope: server-profile ops, Telegram and optional Python workers parked in backlog) | ✅ complete | [checkpoint](migration/checkpoints/M11.md) |
| 12 | M12 — Default cutover and Python removal | ✅ complete | [checkpoint](migration/checkpoints/M12.md) |

## Working agreements that were enforced (for the record)

- Read `AGENTS.md`, `docs/PLAN.md` and the current milestone spec first.
- Every milestone had to produce concrete, auditable evidence (the checkpoint
  docs under `migration/checkpoints/`).
- Task plans and milestone diffs went through an independent read-only review
  gate before merging — self-review did not satisfy it; security/protocol/
  state fixes were mandatory.
- Single source of truth: `Cargo.toml` + `rust-toolchain.toml` versions,
  `cargo fmt`/`clippy -D warnings`, App Protocol as the versioned JSON-RPC 2.0
  contract, all state through `cool-store` migrations at/above the frozen
  `0022` Alembic baseline.
- A milestone that could not proceed for a product/authority blocker (missing
  credentials, unclear requirements, missing secrets) had to surface it
  explicitly instead of papering over it.

## Parked items (still open)

- Optional out-of-process Python workers (document AI, data analysis, export):
  [`docs/backlog/python-workers.md`](backlog/python-workers.md).
- Telegram adapter (M11 §"telegram" profile parked):
  [`docs/backlog/telegram-adapter.md`](backlog/telegram-adapter.md); the
  product-level scope is Phase 5 in [`docs/PLAN.md`](PLAN.md).
- Server-profile (VPS) ops hardening — parked; the packaged runtime stays
  local/single-user per `docs/migration/M2_PACKAGING_CONTRACT.md`.
- Pre-M12 install rollback: [`docs/migration/M12_ROLLBACK.md`](migration/M12_ROLLBACK.md).
