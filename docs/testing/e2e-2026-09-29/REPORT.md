# E2E test pass — v0.2.0 (2026-09-29)

**Scope:** full-stack exercise of the v0.2.0 build — CLI, App Protocol RPC/SSE,
and the web UI in Chrome (recorded). Real LLM: OpenRouter
`stealth/space-bunny-alpha` via `OPENAI_BASE_URL=https://openrouter.ai/api/v1/`.

**Environment:** Windows Server 2022, `cargo build --release -p cool-cli`
(Rust 1.98), `frontend` `npm ci && npm run build` (Vite 8), `cool serve
--data-dir …\cool-e2e-data --bind 127.0.0.1 --port 8000 --assets frontend/dist
--legacy-store --allow-shell` with `SECRET_KEY` set (Fernet keyring).

## Scenario matrix — results

| # | Scenario | Result |
|---|----------|--------|
| C1 | `cargo build --release -p cool-cli` on v0.2.0 tag | **FAILED → fixed** (B0) |
| C2 | `cool doctor` | PASS — full JSON; `jobobject` backend available, `bwrap`/`seatbelt` absent (correct on Windows). Minor: `"phase":"M11"` vs server health `"M12"` (I8) |
| C3 | `cool run --scripted` | PASS — echo driver works offline |
| C4 | `cool run` (real provider, default policy) | PASS as designed — all tools auto-denied (`Decision::Ask` + `AutoApprovalGate::Denied`), model explains denial cleanly |
| C5 | `cool run` + `.cool/policy.json` allow rules | PASS — `read_file`/`write_file`/`search_files` executed; `result.txt` written on disk |
| C6 | `cool run --mode json` | PASS — one NDJSON line per event (`run.started`, `content.delta`, `tool.*`, `usage.updated`, `run.completed`) |
| C7 | `cool run --allow-shell` / `--sandbox jobobject` shell+git | FAIL (known limitation) — `host launcher cannot isolate network access`; **no CLI path to grant network=Allow** (B1) |
| C8 | `cool run` invalid API key | PASS — `{"coolCode":"provider_unauthorized"}` clean fail |
| R1 | `GET /api/health` | PASS — `serverVersion:0.2.0`, `phase:M12`, full capability list |
| R2 | `POST /api/rpc` initialize | PASS — `already_initialized` on repeat (server auto-initializes) |
| R3 | `providers.create` without `SECRET_KEY` | PASS (negative) — `secret_key_unavailable`, fails closed |
| R4 | `providers.create` with keyring | PASS — OpenRouter stored, `apiKeyHint` masked `sk-…cf14` |
| R5 | `session.create` → `session.prompt` → `GET /api/events` SSE | PASS — run.completed with `E2E-OK`; full event chain incl. usage |
| U1 | Settings → Providers | PASS — masked key, live model-list probe, enable+persist |
| U2 | Chat + workspace policy rules | PASS — allowed tools run without prompts; files land on disk |
| U3 | Approval card flow | PASS — diff preview, scope select; "don't ask again → project" persists `.cool/policy.json` |
| U4 | `ask_user` | PASS — question card, answer flows back, run continues |
| U5 | Shell/git in UI (Windows) | FAIL (B1) — same launcher rejection even with `network=Allow` in capability matrix (verified inherited) |
| U6 | Subagents | PASS — background launch, durable run, transcript dialog |
| U7 | Inspector | **FAIL** — run picker always empty, page unusable (B2) |
| U8 | Session fork | PASS w/ bug — history+workspace carried; model/permissions/policy silently dropped (B3) |
| U9 | Pages sweep | PASS — all pages render; **zero JS errors** in console |
| U10 | Multimodal | PASS — attach→chip→send; model read text off the image; download link works |
| U11 | Bad-model resilience | PASS — fast fail, UI usable, switch-back recovers (B5: no reason text) |

## Bugs found

### Critical
- **B0 — v0.2.0 could not build.** `scripts/bump-version.mjs` only rewrote
  `[workspace.package]` + `Cargo.lock`; every `crates/*/Cargo.toml` keeps
  `cool-* = { version = "0.1.0" }` pins, so resolution fails on the bumped
  workspace and the `v0.2.0` tag produced **no release artifacts**.
  **Fixed in this pass:** pins → 0.2.0, script extended to rewrite dep pins.

### Major
- **B1 — shell/git unusable in `cool run` on Windows (and any launcher on
  Windows).** `run_policy` hardcodes wildcard `Ask` → `process_net` →
  `NetAccess::None`; the host backend refuses anything below `Full` and
  JobObject is containment-only → every process spawn fails closed. No flag
  and no `.cool/policy.json` rule can grant the `network` capability (rules
  don't affect capability resolution). Same rejection in the UI even with
  `network=Allow` — the capability matrix setting does not reach the launcher
  decision path. Needs: a capability-grant surface for `cool run`
  (`--allow-network`/`--yes`/profile) + wiring the UI's capability settings
  into `process_net`.
- **B2 — Inspector is dead UI.** Run picker calls legacy `runs.list` → `[]`
  for all conversations while `session.runs` lists the real runs. Timeline /
  Compare / Replay unreachable.
- **B3 — `session.fork` drops settings.** Forked conversation keeps
  `workingDirectory` + history but `model`/`permissions`/`capabilityPolicy`
  become null — parent's safety posture silently lost.
- **B4 — Approval-state traps.** (a) `awaiting_approval` run after page
  reload shows "Awaiting approval" with no card — unrecoverable via UI.
  (b) A client-side-failed `approval.resolve` renders "Denied — the agent was
  notified" while the backend never received a decision.
- **B5 — Directory browser can't drill into folders.** Child paths join as
  `\\?\C:\…/name` (verbatim prefix + mixed separators) → server says "not a
  directory". Blocks the workdir/project pickers (RPC workaround used).

### Medium
- **B6 — `run.failed` hides the reason.** `provider_http` failure renders as
  an empty assistant bubble with just a duration; no toast/inline error.
- **B7 — Analytics & Budgets all zeros** despite ~8 real runs — telemetry not
  fed by the session-run pipeline (same gap class as B2's `runs.list`).
- **B8 — Empty assistant turns** from model flakiness render as silent empty
  bubbles; UI gives no "empty turn" signal. `cool doctor` still says
  `phase:"M11"` (server health says `M12`).

### Minor
- B9–B13: subagent transcript shows `(empty)` for tool messages + `0 tokens`;
  recorder "Completed without tools" mislabels tool runs; Attachments panel
  ignores staged files; Constructor → Blueprints lacks empty-state; occasional
  sidebar/dropdown click-registration quirks (low confidence).

## Evidence

- `screenshots/` — 14 UI captures (providers, tool cards, approval card,
  ask_user, capability matrix, subagent output, empty Inspector, fork,
  multimodal, failed/recovered run).
- Screen recording (full pass, ~23 MB): kept out of the repo —
  `C:\Users\Administrator\screencasts\rec-f435903e-…\…-edited.mp4` on the test
  box, attached to the session.
- `ui-findings.md` — the browser pass write-up (11 scoped areas, per-area
  pass/fail).

## Not exercised

Inspector timeline/compare/replay (B2), Deep Research run, Tasks scheduling,
Memory/Wiki writes, Breakpoints/Rewind, Extensions/MCP install, TUI, ACP
client, OAuth flows (need provider accounts), sandbox backends bwrap/seatbelt
(Linux/macOS only), packaged INSTALL.md path, Docker image.
