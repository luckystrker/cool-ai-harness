# Scope 2 verification — Inspector + Analytics/Budgets data paths (B2, B7, B8a)

Branch: `fix/scope2-data-paths` (commits `a2b50cb`, `79a8157`).

## What changed and why

The session-run pipeline (`session.prompt` / `session.steer` / `session.compact` /
rewind / replay) writes only to `DurableStore` (`rust_*` tables). The Inspector run
picker, its timeline/compare/replay views, and the Analytics + Budgets pages all
read the *legacy* tables (`agent_runs`, `run_events`, `tool_calls`, `spend_log`,
`budgets`) that the old agent loop used to fill. Since the cutover nothing wrote
those tables for conversation chats, so `runs.list` returned `[]` and every
analytics aggregate returned 0.

Fix: a write-through mirror (`crates/cool-app-server/src/legacy/mirror.rs`). Every
canonical `CanonicalEvent` appended for a run whose session is bound to a
conversation (`conversation_id_for_session`) is projected into the legacy read
model inside `AppServer::append_event` and the steer/cancel/disconnect/compaction/
rewind paths. Events for sessions with no conversation link are skipped (the
legacy surfaces are conversation-scoped). A mirrored run row carries
`config.durableRunId`, so the canonical run ↔ legacy row link is inspectable.

- `run.started` → `agent_runs` row (`status running`, model) + `start` event
- `item.completed` → `message` events (user/assistant), reasoning → `thinking`
- `tool.requested/completed/failed` → `tool_call_start`/`tool_result` + `tool_calls` row
- `usage.updated` → `llm_call_complete` event, cumulative `run.usage`,
  `iterations += 1`, and a `spend_log` row (`NewSpendEntry`) — this is what feeds
  `analytics.summary`, `analytics.spend_*`, `budgets.spend`
- `budget.warning/exceeded` → `budget_alert` events
- `run.completed/failed/cancelled` → `finish` event + `finish_run`
- rewind → seed-run events re-projected; superseded rows closed `cancelled`
- `inspector.replay` → legacy row under the `inspector.replay` idempotency key,
  then a canonical auxiliary run is spawned and its events mirror into that row
- `cool doctor` and `/api/health` now share `cool_app_server::PHASE = "M12"` (B8a)

## Repeatable manual check (session.run → Inspector → Analytics)

Prereq: build and serve with the legacy store enabled (the default `cool serve`
wiring), and a browser session with a real provider configured.

1. In the app, open any conversation (or create one) and send a prompt so a
   session run executes. Note the conversation.
2. **Inspector (B2):** open Inspector for that conversation — the run picker
   now lists one row per session run (`runs.list` returns real rows instead of
   `[]`). Pick the run: the Timeline shows `start`, `message`,
   `llm_call_complete`, `finish` entries; Compare works; Replay spawns a new
   mirrored run that appears as a second picker entry.
3. **Analytics (B7):** open Analytics — `total_llm_calls` ≥ 1,
   `total_tokens` > 0, spend-by-model rows appear; open Budgets → spend log
   lists one row per `usage.updated` event with model/tokens/cost.
4. CLI cross-check (optional): `sqlite3` the legacy DB —
   `SELECT status, usage, json_extract(config,'$.durableRunId') FROM agent_runs;`
   shows the canonical run id, and `SELECT * FROM spend_log;` the token counts.
5. **B8a:** `cool doctor` output and `GET /api/health` now report the same
   `"phase":"M12"`.

## Automated evidence

- `cargo test -p cool-app-server --test chat_cutover` — 10/10, incl.:
  - `session_run_mirrors_into_legacy_inspector_and_analytics`: prompt →
    `session.runs` has the canonical run, `runs.list` returns the mirrored
    `completed` row with `config.durableRunId` = canonical run id and merged
    usage (20 total tokens), `inspector.timeline` exposes
    start/message/llm_call_complete/finish, `analytics.summary` returns
    `total_llm_calls=1`, `total_tokens=20`, `budgets.spend` returns the spend
    row with `model="scripted"`.
  - `inspector_replay_spawns_a_mirrored_agent_run`: `inspector.replay` returns
    `status=running`, the spawned canonical run binds the pre-created row via
    `durableRunId` and drives it to `completed`; a repeated call replays the
    idempotent record instead of spawning again.
- `cargo test -p cool-store --test runs` — 5/5, incl.
  `run_progress_durable_binding_and_timestamped_events_support_the_mirror`
  (binding, backfilled timestamps, usage merge, non-terminal guard).

## Gates (all green on this branch)

- `cargo fmt` — clean
- `cargo clippy -p cool-app-server -p cool-store -p cool-agent -- -D warnings` — clean
- `cargo test -p cool-app-server -p cool-store -p cool-cli` — all suites pass
- `npm run lint` (frontend) — clean (3 pre-existing fast-refresh warnings)
- `npm run build` (frontend) — builds successfully
- Protocol untouched → `generate --check` / `protocol:check` not required.
