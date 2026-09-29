# Scope 2 verification — Inspector + Analytics/Budgets data paths (B2, B7, B8a)

Branch: `fix/scope2-data-paths` (iteration-2 review fixes added on top of
`a2b50cb`, `79a8157` + iteration-1).

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
model inside `AppServer::append_event` and the steer/cancel/disconnect/rewind
paths. Events for sessions with no conversation link are skipped (the legacy
surfaces are conversation-scoped). A mirrored run row carries
`config.durableRunId`, so the canonical run ↔ legacy row link is inspectable, and
`config.mirroredSeq` records the projection watermark.

- `run.started` → `agent_runs` row (`status running`, model) + `start` event
- `item.completed` → `message` events (user/assistant), reasoning → `thinking`
- `tool.requested/completed/failed` → `tool_call_start`/`tool_result` + `tool_calls` row
- `tool.approval_required/resolved` → `tool_approval_request`/`tool_approval_resolved`
  (the legacy kinds the frontend union in `frontend/src/api/types.ts` expects)
- `usage.updated` → `llm_call_complete` event, cumulative `run.usage`,
  `iterations += 1`, and a `spend_log` row (`NewSpendEntry`) — this is what feeds
  `analytics.summary`, `analytics.spend_*`, `budgets.spend`
- `budget.warning/exceeded` → `budget_alert` events
- `run.completed/failed/cancelled` → `finish` event + `finish_run`
- `inspector.replay` → legacy row under the `inspector.replay` idempotency key,
  then a canonical auxiliary run is spawned and its events mirror into that row;
  every call re-drives the spawn path (`inspector.replay.exec` makes it
  self-idempotent), so a failed spawn retries instead of reporting a phantom run
- `cool doctor` and `/api/health` share `cool_app_server::PHASE = "M12"` (B8a)

Correctness guards added in the review iteration:

- **Rewind copies don't re-count.** `session.rewind` clones the retained
  history into the seed run with `extensions.rewound = true`. The mirror emits
  only the `run_events` timeline rows for those copies and skips
  `spend_log`/`tool_calls`/`usage`/`finish_run` — the superseded run's rows
  already recorded them.
- **Crash recovery reaches the mirror.** `recover_incomplete_runs`' emitted
  envelopes (`run.failed`/`tool.failed`) are mirrored in `with_store` /
  `with_agent_runtime`, so a run abandoned mid-`running` closes `failed`
  instead of staying a zombie picker entry.
- **Startup reconciliation sweep.** `reconcile_legacy_mirror` runs at server
  build: it walks every conversation-linked session's canonical runs, replays
  events above each row's `mirroredSeq` watermark (backfilling runs that
  predate the mirror), and force-closes mirrored rows still open whose
  canonical run is terminal. The same per-session reconcile
  (`reconcile_linked_session`) also runs when a conversation binds its
  durable session after startup, so pre-link runs do not wait for a restart.
- **Auxiliary runs never mirror.** Every canonical run carries a `purpose`
  tag at creation (`rust_runs.purpose`): `compact` and `subagent`
  bookkeeping runs are skipped by the sweep even while still running
  (`finish_reason` `import`/`compact`/`subagent_*` covers rows written
  before the column existed); `replay_exec` and `research_exec` runs are
  user-facing and keep mirroring, as do crash-recovered real runs
  (`core_restarted`). Pre-purpose running aux rows can not be told apart
  from real runs — that window is tiny and self-heals at terminal.
- **Bounded sweep.** Each run replays at most 4096 events per pass
  (`MIRROR_SWEEP_EVENTS_PER_RUN`) — a deeper backlog resumes from
  `mirroredSeq` on the next pass instead of blocking boot or link.
- **Watermark dedupe is monotonic.** `project()` skips any envelope with
  `seq <= mirroredSeq`, so overlapping write paths (live hook ↔ sweep ↔
  recovery mirror) can't double-project an event, and
  `set_run_mirror_cursor` uses `MAX(existing, new)` so an out-of-order
  projection can't regress the watermark.
- **Projection errors warn, never silently drop.** `tracing::warn!` on every
  mirror failure; the canonical append is unaffected either way.
- **Atomic row creation.** `ensure_run_by_durable_id` uses
  `INSERT ... WHERE NOT EXISTS`, so racing first-events can't duplicate a
  picker row.
- **Finished rows can't reopen.** `update_run_progress` carries
  `WHERE finished_at IS NULL`.
- **Steer mirrors the exact envelope.** The steer path fetches the appended
  event by the seq `SteerAcceptedResult` reports instead of re-reading the
  tail, so an interleaved append can't lose the steer message.

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

## Scripted-driver variant (no API keys needed)

Every path above is covered by in-process tests on `ScriptedDriver` + two
`in_memory` stores — no provider credentials required:

- `rewind_mirrors_copied_history_without_double_counting` — scripted
  `read_file` tool call + usage, then `session.rewind` through
  `Command::SessionRewind`: the seed run's copied `tool.completed`/messages
  produce timeline rows but `tool_calls`/`spend_log`/`run.usage` counts stay
  unchanged.
- `recovered_crash_run_closes_its_mirrored_row` — a durable `running` run
  seeded before `AppServer::with_store` is mirrored `failed` by recovery.
- `startup_sweep_backfills_unmirrored_runs` — a completed canonical run
  written straight to `DurableStore` (no mirror hook) is fully backfilled by
  the startup sweep: row, usage, spend, and the
  start/message/llm_call_complete/finish timeline. It also seeds
  `rewound`-tagged `usage.updated`/`tool.completed` copies and asserts they
  add no `spend_log`/`tool_calls`/`usage` rows — the copied-usage invariant
  locked directly (copied kinds are unconstrained by `is_history_event`).
- `startup_sweep_skips_auxiliary_runs` — `subagent` and `compact` purpose
  runs (with usage events) produce no `agent_runs` row and no spend;
  a `replay_exec` purpose run mirrors normally.
- `inspector_replay_spawns_a_mirrored_agent_run` — replay bookkeeping +
  spawn; a repeat call replays the idempotent record and still reaches the
  self-idempotent exec gate (no zombie `running` bookkeeping rows).
- `run_progress_durable_binding_and_timestamped_events_support_the_mirror`
  (cool-store) — `ensure_run_by_durable_id` find-or-create, `mirroredSeq`
  round-trip, and the `finished_at IS NULL` guard: `update_run_progress` can
  no longer reopen a completed row.

## Automated evidence

- `cargo test -p cool-app-server --test chat_cutover` — all pass, incl.:
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
  - `rewind_mirrors_copied_history_without_double_counting`,
    `recovered_crash_run_closes_its_mirrored_row`,
    `startup_sweep_backfills_unmirrored_runs` (above).
- `cargo test -p cool-store --test runs` — all pass, incl.
  `run_progress_durable_binding_and_timestamped_events_support_the_mirror`
  (binding, backfilled timestamps, usage merge, non-terminal guard,
  `ensure_run_by_durable_id` dedupe, finished-row reopen guard, cursor
  watermark).

## Gates (all green on this branch)

- `cargo fmt` — clean
- `cargo clippy -p cool-app-server -p cool-store -p cool-agent --all-targets -- -D warnings` — clean
- `cargo test -p cool-app-server -p cool-store -p cool-cli` — all suites pass
- `npm run lint` (frontend) — clean (3 pre-existing fast-refresh warnings)
- `npm run build` (frontend) — builds successfully
- Protocol untouched → `generate --check` / `protocol:check` not required.
