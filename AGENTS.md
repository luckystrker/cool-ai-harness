# AGENTS.md

Guidance for AI coding agents working in this repository. Read this before
editing. Cool AI Harness is a personal AI agent harness: the **Rust trusted
core is the runtime** — the `cool` binary serves the React/TypeScript SPA, the
canonical App Protocol, the agent loop, tools, subagents and deep research. The
Python/FastAPI backend was removed under
[`docs/migration/adr/0003-remove-legacy-python-server.md`](docs/migration/adr/0003-remove-legacy-python-server.md)
(M12); optional out-of-process workers are specified in
[`docs/backlog/python-workers.md`](docs/backlog/python-workers.md), and rollback
for pre-removal installs is documented in
[`docs/migration/M12_ROLLBACK.md`](docs/migration/M12_ROLLBACK.md). The product
roadmap lives in [`docs/PLAN.md`](docs/PLAN.md). Phases 0–4 (foundation,
agent loop, reliability/security, skills/MCP/subagents, memory/personalities/
tasks, workflows/multimodal/code tools) plus the 0.2 hardening track are
done. Telegram (Phase 5) is still an empty placeholder.

The completed architecture-migration roadmap is
[`docs/RUST_CORE_MIGRATION_PLAN.md`](docs/RUST_CORE_MIGRATION_PLAN.md): a Rust trusted core,
versioned App Protocol, Rust TUI, native Skills/MCP/hooks, and isolated compatibility workers.

## Repository layout

Two source roots live in one repo:

- `crates/` — the Rust trusted core workspace (`cool-protocol`, `cool-state`,
  `cool-store`, `cool-agent`, `cool-cli`, `cool-tui`, `cool-acp`,
  `cool-extensions`, `cool-app-server`, `cool-http`, `cool-security`).
- `frontend/src` — the React SPA (Vite + TypeScript).

`crates/cool-store` owns the SQLite schema at the frozen Alembic baseline
`0022`, takes a verified backup before adopting a pre-M12 `harness.db`, owns
Rust migrations (`rust_store_meta`, `rust_idempotency`), and exposes typed
subsystem stores. The typed legacy command families live in
`crates/cool-protocol/src/families/`, their dispatch in
`crates/cool-app-server/src/legacy/`, and the frontend contract gate is
`frontend/protocol-tests/{inventory.json,coverage.ts}` with the typed SDK in
`sdk/typescript/src/client.ts`. The app server serves the legacy families only
when `ServerConfig::legacy_store` is set (CLI: `cool serve --legacy-store`,
read-only unless the file is already Rust-owned).

`crates/cool-http` is the browser-facing HTTP/SSE projection of the App
Protocol (`cool serve`). It owns no business logic — each browser identity is
one in-process `AppServer::serve_io` connection driven by `AppClient`;
`POST /api/rpc` carries canonical `RpcRequest`/`ServerFrame` and
`GET /api/events` is the canonical cursor/reconnect SSE stream. The `local`
(loopback, optional token) and `server` (token + TLS/reverse-proxy boundary)
profiles are validated at startup and fail closed. `sdk/typescript/src/http.ts`
adds the fetch/SSE `CoolTransport`. The experimental OpenCode Bun compatibility
worker (`crates/cool-extensions/src/opencode.rs`) is opt-in via
`COOL_OPENCODE_WORKER`; the Telegram adapter is parked in
[`docs/backlog/telegram-adapter.md`](docs/backlog/telegram-adapter.md).

Supporting roots: `sdk/typescript` (generated typed client), `schemas/`
(protocol JSON schemas), `skills/` (bundled SKILL.md skills), `docs/` (roadmap,
migration evidence, backlog). Run the command for **every root you
touched** before declaring a task done (see [Definition of done](#definition-of-done)).

## Cross-cutting architecture constraints

These come from [`docs/PLAN.md`](docs/PLAN.md) "Architectural principles";
do not violate them without an explicit decision:

- **Provider abstraction.** All LLM access goes through the single provider
  interface in `crates/cool-agent` (`ModelProvider`/`ScriptedDriver`). Never
  call an LLM SDK directly from the agent loop, tools, or the protocol layer.
- **Streaming-first.** LLM calls stream tokens; interactive runs are
  cancellable through the run registry and the canonical `run.cancel` command.
- **Pluggable registries.** Tools, skills, MCP servers, and subagent roles are
  registries/plugins. New tools are registered, not hard-wired into the loop.
- **Capability security model.** Permissions split into
  `read`/`write`/`execute`/`network`/`git`/`send_external` (see
  `crates/cool-security/`). File tools are confined to allowed workspaces;
  network tools use an allowlist with size/time limits and SSRF protection;
  code execution is sandboxed without access to host secrets; secrets are
  masked in messages, traces, and logs.
- **Durable execution.** Every agent turn is a run with an append-only
  `run_events` log, status (`running`/`awaiting_approval`/`completed`/`failed`/
  `cancelled`), cumulative token/cost usage, a checkpoint after each tool call,
  and budget guards. Subagent, planning, and scheduled (cron) runs are durable
  too. Do not add side effects outside a run's event log.
- **Memory is append-first and project-scoped.** Long-term memory lives in
  `crates/cool-store/src/memory.rs` (items/episodes/working memory, plus entity
  extraction); recall is FTS5 + composite reranking,
  extraction/decay/consolidation are background sweeps. Memory visibility is
  keyed by the working directory (`_project_key`). The agent reaches memory only
  through registered memory tools — never write to the memory tables directly
  from the loop.
- **Observability = the event log, not side channels.** The inspector
  (`crates/cool-store/src/observability.rs`) reconstructs
  timelines/comparisons/replay from `run_events`; analytics aggregate
  spend/tool/runs stats from the DB. Prefer emitting an event over adding a
  separate logging path.
- **Context window is budgeted.** Token estimation and history truncation live
  in `crates/cool-agent` (context budgeting, AGENTS.md-style project
  instructions, compaction). Keep token estimation in one place when adding
  prompt content.
- **The Rust store owns the schema.** `crates/cool-store` is the only writer of
  `harness.db`; schema changes ship as Rust migrations at/above the frozen
  `0022` baseline. A pre-adoption database opens read-only — never bypass the
  ownership guard.
- **API contract is the App Protocol.** `crates/cool-protocol` defines the
  command union consumed by `sdk/typescript` and `frontend/src/api/*`. Regenerate
  bindings with `cargo run -p cool-protocol --bin generate` and keep schema,
  SDK and dispatch arms in sync.

### Secrets, env, and data

- `.env` is gitignored (only root `.env.example` is tracked). API keys are
  encrypted at rest by `crates/cool-security` (Fernet). **Never commit `.env`,
  `*.db`, or runtime data.**
- Runtime artifacts (`data/`, `workspaces/`) are gitignored — this includes the
  SQLite DBs, working-memory scratchpads, memory FTS index, and artifact
  storage.
- To configure locally: `cp .env.example .env` and set at least
  `OPENAI_API_KEY` (or `OPENAI_BASE_URL` for an OpenAI-compatible backend) plus
  a `SECRET_KEY` Fernet key.

## `frontend/src`

### Layout

React 19 + TypeScript + Vite 8 + Tailwind 4. State via `zustand`, server cache
via `@tanstack/react-query`, routing via `react-router-dom`. Markdown rendering
via `react-markdown` (+ `remark-gfm`, `rehype-highlight`), toasts via `sonner`.
UI primitives in `src/components/ui` (Radix-based).

```
frontend/src/
├── main.tsx, App.tsx        # entry + routing
├── api/                    # typed boundary to backend — one client per subsystem
│                           #   (types, streaming, client, conversations, providers, settings,
│                           #    mcp, memory, plans, skills, subagents, inspector, budgets,
│                           #    artifacts, workspace, profiles, analytics, tasks, rss,
│                           #    webhooks, wiki)
├── hooks/                  # useConversationStream.ts (SSE)
├── components/
│   ├── chat/               # MessageBubble, ToolCallBlock, ApprovalCard (write diffs),
│   │                       #   PlanCard, ArtifactPanel, BudgetIndicator, ThinkingBlock,
│   │                       #   ProfileSwitcher, ComposerToolbar, DirectoryBrowserDialog,
│   │                       #   ProjectDialog, ProjectSettingsDialog, Markdown, ...
│   ├── memory/             # EntitiesPanel, ExplainPanel, ReviewQueue
│   ├── inspector/          # RunTimeline, ComparisonView
│   ├── subagents/          # LaunchForm, RoleEditor, RunCard, SubagentOutputDialog
│   ├── settings/           # ChatModelsPicker
│   ├── layout/             # AppLayout, Sidebar
│   └── ui/                 # Radix-based primitives (button, card, dialog, ...)
├── pages/                  # ChatPage, MemoryPage, WikiPage, ProfilesPage, AnalyticsPage,
│                           #   TasksPage, SettingsPage, BudgetsPage, SubagentsPage,
│                           #   InspectorPage
├── lib/                    # utils, queryClient, agentConfig, modelFormat, projects
└── assets/
```

### Conventions

- Path alias `@/*` → `./src/*` (defined in `tsconfig.app.json`, matches the Vite
  `resolve.alias`). Prefer `@/...` imports.
- `tsconfig.app.json` enforces `noUnusedLocals`, `noUnusedParameters`,
  `noFallthroughCasesInSwitch`, `verbatimModuleSyntax`, `erasableSyntaxOnly`.
- oxlint config (`.oxlintrc.json`): plugins `react`, `typescript`, `oxc`;
  `react/rules-of-hooks` is an error, `react/only-export-components` is a warn.
- `src/api/*` is the **only** typed boundary to the server — every op maps to
  a canonical App Protocol command or a documented transport exception
  (`frontend/protocol-tests/inventory.json`).

### Required commands (run from `frontend/`)

```bash
npm install               # install deps
npm run lint              # oxlint (required)
npm run build             # tsc -b && vite build — typecheck + production build (required)
npm run dev               # vite dev server with hot reload
npm run preview           # preview the production build
```

> On Windows PowerShell, if `npm`/`npx` resolve to blocked `.ps1` shims, invoke
> `npm.cmd` / `npx.cmd` directly (e.g. `npm.cmd run lint`).

### Co-change expectations

- `src/api/types.ts` ↔ `crates/cool-protocol` — the wire shapes are generated
  from the protocol schema; keep types aligned with the generated SDK.
- `src/hooks/useConversationStream.ts` ↔ the canonical event stream
  (`GET /api/events` / `run.subscribe` fan-out in `crates/cool-http`).
- A new server event → add a handler in `useConversationStream.ts` and
  render it in `src/components/chat/*`.
- A new server subsystem → add a client in `src/api/`, types in `types.ts`,
  and a page/component to surface it.
- A change to `src/api/*.ts` operations → keep `protocol-tests/inventory.json`
  and `sdk/typescript/src/client.ts` in sync; `npm run protocol:check` fails on
  undeclared operations, phantom commands and missing dispatch arms.
- Shared UI primitives in `src/components/ui` are consumed across `chat/` —
  don't break existing consumers when editing them.

## Definition of done

Before declaring a task complete:

1. Run the commands for every root you touched.
2. After implementation changes are finished, request an independent, read-only code review from
   a reviewer/agent that did not author those changes. The review must inspect the actual diff and
   relevant untracked files, not only the implementation summary.
3. Resolve every actionable finding by fixing it or recording an evidence-backed rejection. After
   review-driven fixes, re-run the affected checks and request another independent pass for changes
   to security boundaries, protocol/state semantics, migrations, or other high-risk behavior.
4. Record the independent review result and any accepted residual risks in the phase checkpoint or
   final task report. A self-review does not satisfy this gate.

Required root commands:

- Touched the production Rust workspace (`Cargo.toml`, `crates/`, protocol schema/generator)?
  From the repository root:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
  cargo build --workspace --all-targets
  cargo run -p cool-protocol --bin generate -- --check
  ```

- Touched `frontend/`? From `frontend/`:
  ```bash
  npm run protocol:check && npm run lint && npm run build
  ```
  `protocol:check` also typechecks the SDK client (`sdk/typescript`) and runs the
  inventory coverage gate.
- Touched release packaging (`Dockerfile`, `docker-compose.yml`, `packaging/`, or the runtime path/
  entrypoint contract)? From the repository root:
  ```bash
  docker compose config --quiet
  docker build --tag cool-ai-harness:local .
  ```
  Then start the image and smoke `/`, `/api/health`, and SSE. If a local Docker daemon is
  unavailable, record that the image build remains CI-only evidence; do not report a local image
  build as passed.
- Touched the **API contract** (protocol schema/commands/events)? Update the
  schema, the generated SDK bindings and the dispatch arms, then re-run the
  generator check plus the frontend `protocol:check`.
