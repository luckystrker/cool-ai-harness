# backend/tests — legacy-lane suite (post-M12 disposition)

As of M12 (`docs/migration/checkpoints/M12.md`) the **Rust trusted core is the
default runtime** and no SPA/backend HTTP surface this suite exercises is on
the production path. This suite is kept — not deleted — as the regression net
for the legacy Python lane until
[`docs/migration/adr/0003-remove-legacy-python-server.md`](../../docs/migration/adr/0003-remove-legacy-python-server.md)
executes.

What replaced it for the default runtime:

- `crates/cool-store/tests/` — per-domain store parity (conversations, runs,
  memory, research, subagents, tasks, rss, webhooks, wiki, plans, budgets,
  analytics, providers, profiles, observability, artifacts, scheduler,
  constructor, adoption).
- `crates/cool-app-server/tests/` — protocol surface (`legacy_surface.rs`
  covers every legacy family end-to-end through the client, `server.rs`,
  `sessions.rs`, `chat_cutover.rs`, `subagent_executor.rs`,
  `plan_execution.rs`, `conversation_compaction.rs`, `durable_restart.rs`,
  `local_transport.rs`).
- `crates/cool-agent/tests/` — agent loop, trusted tools, deterministic evals.
- `crates/cool-http/tests/http_facade.rs` + `frontend/protocol-tests/` — the
  HTTP/SSE facade and the SDK coverage/inventory gate.
- `tests/test_rust_store_contract.py` (this directory) — the cross-language
  store contract; keep green.

Rules:

- New coverage for the default runtime goes to the Rust suites, not here.
- Changes inside `backend/` still require this suite to pass
  (`pytest -q -n auto --dist=loadfile`), plus `python -m evals`.
- When ADR-0003 executes, the files that only cover the deleted server are
  removed with it; `test_rust_store_contract.py` and the evals harness are
  re-pointed or moved per the ADR's "Kept" section.
