# AGENTS.md

Guidance for AI coding agents working in this repository. Read this before
editing. Cool AI Harness is a personal AI agent harness: a Python/FastAPI
backend plus a React/TypeScript SPA, following a phased roadmap in
[`docs/PLAN.md`](docs/PLAN.md) and [`docs/phases/`](docs/phases/). Phases 0–3b
and **Фаза 4** (Deep Research, Code/Git, multimodal, browser automation,
Agent Constructor) are done. Telegram (Фаза 5) is still an empty placeholder.

The active architecture-migration roadmap is
[`docs/RUST_CORE_MIGRATION_PLAN.md`](docs/RUST_CORE_MIGRATION_PLAN.md). It replaces the earlier
TypeScript-core proposal with a Rust trusted core, versioned App Protocol, Rust TUI, native
Skills/MCP/hooks, and isolated TypeScript/Python compatibility workers. Until a migration phase
passes its checkpoint, the current Python constraints in this file remain authoritative.

## Repository layout

Two source roots live in one repo:

- `backend/app` — the FastAPI application (Python package `app`).
- `frontend/src` — the React SPA (Vite + TypeScript).

Supporting roots: `backend/tests` (pytest suite), `backend/evals`
(scenario-driven agent evals / CI gate), `backend/alembic` (DB migrations),
`docs/` (roadmap + per-phase specs), and `spikes/m0-rust-core` (the isolated,
non-production Rust M0 evidence harness). Run the command for **every root you
touched** before declaring a task done (see [Definition of done](#definition-of-done)).

## Cross-cutting architecture constraints

These come from [`docs/PLAN.md`](docs/PLAN.md) "Архитектурные принципы" and the
phase specs; do not violate them without an explicit decision:

- **Provider abstraction.** All LLM access goes through the single
  `LLMProvider` interface in `backend/app/providers/base.py`. Never call an LLM
  SDK directly from the agent loop, tools, or API layer.
- **Streaming-first.** LLM calls stream tokens via SSE/WebSocket. Interactive
  runs are cancellable through the in-process `run_registry`.
- **Pluggable registries.** Tools, skills, MCP servers, and subagent roles are
  registries/plugins. New tools are registered, not hard-wired into the loop.
- **Capability security model.** Permissions split into
  `read`/`write`/`execute`/`network`/`git`/`send_external` (see
  `backend/app/security/`). File tools are confined to allowed workspaces;
  network tools use an allowlist with size/time limits and SSRF protection;
  code execution is sandboxed without access to host secrets; secrets are
  masked in messages, traces, and logs.
- **Durable execution.** Every agent turn is an `AgentRun` with an append-only
  `run_events` log, status (`running`/`awaiting_approval`/`completed`/`failed`/
  `cancelled`), cumulative token/cost usage, a checkpoint after each tool call,
  and budget guards. Subagent, planning, and scheduled (cron) runs are durable
  too. Do not add side effects outside a run's event log.
- **Memory is append-first and project-scoped.** Long-term memory lives in
  `backend/app/memory/` (`MemoryItem`/`Episode`/`WorkingMemory`, plus entity
  extraction); recall is FTS5 + composite reranking,
  extraction/decay/consolidation are background sweeps. Memory visibility is
  keyed by the working directory (`_project_key`). The agent reaches memory only
  through registered memory tools — never write to the memory tables directly
  from the loop.
- **Observability = the event log, not side channels.** The run inspector
  (`backend/app/observability/`) reconstructs timelines/comparisons/replay from
  `run_events`; `backend/app/analytics/` aggregates spend/tool/runs stats from
  the DB. Prefer emitting an event over adding a separate logging path.
- **Context window is budgeted.** `agent/context_window.py` estimates tokens
  and truncates history within a budget; `agent/project_instructions.py` loads
  AGENTS.md-style project instructions from the working directory; context
  compaction collapses old turns. Keep token estimation in one place when
  adding prompt content.
- **Models are the source of truth in dev/tests** (`SQLModel.create_all`); in
  production the app applies Alembic migrations on startup. Any change to
  `backend/app/models/*.py` **must** ship a matching migration in
  `backend/alembic/versions/`.
- **API contract is shared.** `backend/app/api/schemas.py` and the
  `app/api/*_router.py` files define the contract consumed by
  `frontend/src/api/types.ts`. WebSocket/SSE event shapes in
  `backend/app/agent/events.py` + `backend/app/api/websocket.py` are mirrored by
  `frontend/src/api/streaming.ts` and `frontend/src/hooks/useConversationStream.ts`.
  Keep both sides in sync.

### Secrets, env, and data

- `.env` is gitignored (only root `.env.example` is tracked). API keys are
  encrypted at rest via `backend/app/core/security.py` (Fernet). **Never commit
  `.env`, `*.db`, or runtime data.**
- Runtime artifacts (`data/`, `workspaces/`, `evals_data*/`) are gitignored —
  this includes the SQLite DB, working-memory scratchpads, memory FTS index,
  artifact storage, and eval baselines/traces.
- To configure locally: `cp .env.example .env` and set at least
  `OPENAI_API_KEY` (or `OPENAI_BASE_URL` for an OpenAI-compatible backend) plus
  a `SECRET_KEY` Fernet key.

## `backend/app`

### Layout

```
backend/app/
├── main.py            # create_app(): routers under /api, WS at /ws; lifespan runs init_db()
├── core/              # config (pydantic-settings), db, logging, security (Fernet)
├── providers/         # LLMProvider + OpenAI/Anthropic impls, registry, resilience, pricing
├── agent/             # loop: executor, runners, service, events, runs, approvals,
│                      #       permissions, planning, subagents, personalities,
│                      #       context_window, project_instructions
├── security/          # capability policy, SSRF, secrets, sandbox, breakpoints, cost
├── tools/             # tool registry + builtins (files, code, bash, git, github, web,
│                      #   mcp, memory, skills, plan, subagent, task, rss, wiki, context)
├── skills/            # skill registry + discovery + TF-IDF/embedding matching (Фаза 2)
├── mcp/               # MCP client (stdio + HTTP), registry, marketplace, tool bridge (Фаза 2)
├── memory/            # long-term + working memory: extractor, retrieval (FTS5),
│                      #   entities, context_builder, lifecycle, tools (Фаза 3a)
├── observability/     # run inspector: live tail, timeline, compare, replay
├── analytics/         # aggregating dashboards, LLM-call log, OTel export (Фаза 3a)
├── budgets/           # spend/budget service
├── artifacts/         # content-addressed artifact storage
├── tasks/             # recurring tasks: scheduler, cron, delivery, templates (Фаза 3b)
├── rss/               # RSS aggregator: subscriptions, fetch, summarize (Фаза 3b)
├── webhooks/          # webhook router (Фаза 3b)
├── wiki/              # wiki article store + tools
├── api/               # HTTP + WebSocket routes + schemas
├── models/            # SQLModel tables
└── telegram/          # bot + web app (EMPTY — Фаза 5)
```

> `telegram/` is still an empty placeholder (0-byte `__init__.py`); its deps
> are listed in `pyproject.toml` but no code exists.

### Conventions

- Tests use the deterministic `ScriptedProvider` (see `backend/tests/conftest.py`)
  — **no API keys needed**. `conftest.py` redirects `DATABASE_URL` to a
  throwaway temp DB *before* any `app.*` import; do not import `app` first or
  you will pollute `data/harness.db`.
- `asyncio_mode = "auto"` (from `[tool.pytest.ini_options]`); async tests need
  no `@pytest.mark.asyncio`.
- Use the `workspace` fixture for file-tool tests so they stay isolated under
  `tmp_path`.
- Ruff config: line-length 100, `target-version = "py312"`, rule set
  `E,F,I,B,UP,SIM,RUF` with `E501` and `B008` ignored. `isort` knows
  `app` as first-party.

### Required commands (run from `backend/`)

```bash
pip install -e ".[dev]"          # install backend + dev deps
ruff check .                     # lint (required)
ruff format .                    # format (optional)
mypy app                         # typecheck (dev dep; strict=false)
pytest                           # run the ~725-test suite (required)
pytest tests/test_agent.py -v    # single file
pytest -n auto                   # run tests in parallel (xdist)
python -m evals                  # agent eval CI gate: exit 0 pass / 1 regression / 2 config
pytest tests/test_evals.py -v   # evals as pytest
alembic upgrade head             # apply migrations
alembic revision --autogenerate -m "describe change"   # new migration after model edits
```

### Co-change expectations (from git history)

- `agent/executor.py` + `agent/runners.py` → update `tests/test_agent.py`.
  Every agent-loop change in history shipped with agent tests.
- `agent/context_window.py` / `agent/project_instructions.py` (context budget,
  AGENTS.md loading, compaction) → update `tests/test_agent.py`; UI in
  `frontend/src/components/chat/` (collapsible history, composer toolbar).
- `agent/planning.py` → update `tests/test_planning.py`; UI in
  `frontend/src/components/chat/PlanCard.tsx` + `frontend/src/api/plans.ts`.
- `agent/subagents.py` → update `tests/test_subagents.py`; UI in
  `frontend/src/components/subagents/` + `frontend/src/pages/SubagentsPage.tsx`.
- `agent/personalities/` (multi-profile agents) → update
  `tests/test_profiles.py`; UI in `frontend/src/pages/ProfilesPage.tsx` +
  `frontend/src/api/profiles.ts` + `frontend/src/components/chat/ProfileSwitcher.tsx`.
- `memory/*` (service, retrieval, extractor, entities, lifecycle, tools) →
  update `tests/test_memory.py`; UI in `frontend/src/pages/MemoryPage.tsx` +
  `frontend/src/api/memory.ts` + `frontend/src/components/memory/`
  (EntitiesPanel, ExplainPanel, ReviewQueue). Memory is reached only via
  `memory/tools.py`.
- `analytics/*` + `api/analytics.py` → update `tests/test_analytics.py`; UI in
  `frontend/src/pages/AnalyticsPage.tsx` + `frontend/src/api/analytics.ts`.
- `tasks/*` (scheduler, cron, delivery, templates) → update
  `tests/test_tasks.py`; UI in `frontend/src/pages/TasksPage.tsx` +
  `frontend/src/api/tasks.ts`.
- `rss/*` → update `tests/test_rss.py`; UI/API in `frontend/src/api/rss.ts`.
- `webhooks/*` → update `tests/test_webhooks.py`; UI/API in
  `frontend/src/api/webhooks.ts`.
- `wiki/*` → UI/API in `frontend/src/pages/WikiPage.tsx` +
  `frontend/src/api/wiki.ts`.
- `skills/*` → update `tests/test_skills.py`; UI/API in
  `frontend/src/api/skills.ts`.
- `mcp/*` → update `tests/test_mcp.py`; UI/API in
  `frontend/src/api/mcp.ts`.
- `observability/*` (inspector) → update `tests/test_inspector.py`; UI in
  `frontend/src/pages/InspectorPage.tsx` + `frontend/src/components/inspector/`.
- `budgets/*` → update `tests/test_budgets.py`; UI in
  `frontend/src/pages/BudgetsPage.tsx` + `frontend/src/api/budgets.ts`.
- `artifacts/*` → update `tests/test_artifacts.py`.
- `tools/*` — git/code/bash tools → update `tests/test_git_tools.py` /
  `tests/test_tools.py` / `tests/test_sandbox.py` (code execution requires
  sandboxing and capability checks).
- `app/models/*.py` → add a `backend/alembic/versions/*.py` migration
  (autogenerate it; verify with `alembic upgrade head`). Current head is
  `0022_phase4_completion`.
- `app/api/schemas.py` or `app/api/*_router.py` → update
  `frontend/src/api/types.ts` and the consuming hook/component.
- `app/agent/events.py` + `app/api/websocket.py` → update
  `frontend/src/api/streaming.ts` and `frontend/src/hooks/useConversationStream.ts`.
- `app/security/*` or `app/tools/*` → adjust the matching tests
  (`test_permissions.py`, `test_tools.py`, `test_sandbox.py`, `test_ssrf.py`,
  `test_secret_masking.py`, `test_capabilities.py`).
- `providers/*` (openai, anthropic, resilience, pricing, registry) → update
  `test_anthropic_provider.py`, `test_providers_api.py`, `test_pricing.py`,
  `test_resilience.py`; UI lives in `frontend/src/pages/SettingsPage.tsx`.

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
├── hooks/                  # useConversationStream.ts (SSE/WS)
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
- `src/api/*` is the **only** typed boundary to the backend — keep it in sync
  with `backend/app/api`.

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

- `src/api/types.ts` ↔ `backend/app/api/schemas.py` — keep shapes in sync. Each
  subsystem also has its own client (`src/api/{memory,plans,subagents,tasks,
  rss,webhooks,wiki,profiles,analytics,...}.ts`) mirroring the matching
  `app/api/*_router.py`.
- `src/hooks/useConversationStream.ts` ↔ `backend/app/api/websocket.py` +
  `backend/app/agent/events.py`.
- A new backend SSE/WS event → add a handler in `useConversationStream.ts` and
  render it in `src/components/chat/*`.
- A new backend subsystem → add a client in `src/api/`, types in `types.ts`,
  and a page/component to surface it.
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

- Touched `backend/`? From `backend/`:
  ```bash
  ruff check . && mypy app && pytest && python -m evals
  ```
- Touched `frontend/`? From `frontend/`:
  ```bash
  npm run lint && npm run build
  ```
- Touched `spikes/m0-rust-core/`? From that directory:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-features
  cargo build --all-targets
  ```
- Touched the **API contract** (schemas/events/WebSocket)? Update **both**
  sides and re-run both root command sets.
- Touched `app/models/*.py`? Add the Alembic migration and verify
  `alembic upgrade head`.
