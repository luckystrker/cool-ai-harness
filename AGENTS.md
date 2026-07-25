# AGENTS.md

Guidance for AI coding agents working in this repository. Read this before
editing. Cool AI Harness is a personal AI agent harness: a Python/FastAPI
backend plus a React/TypeScript SPA, following a phased roadmap in
[`docs/PLAN.md`](docs/PLAN.md) and [`docs/phases/`](docs/phases/) (currently
Phase 1.5 — durable runs, capability security, artifacts, evals).

## Repository layout

Two source roots live in one repo:

- `backend/app` — the FastAPI application (Python package `app`).
- `frontend/src` — the React SPA (Vite + TypeScript).

Supporting roots: `backend/tests` (pytest suite), `backend/evals`
(scenario-driven agent evals / CI gate), `backend/alembic` (DB migrations),
`docs/` (roadmap + per-phase specs). Run the command for **every root you
touched** before declaring a task done (see [Definition of done](#definition-of-done)).

## Cross-cutting architecture constraints

These come from [`docs/PLAN.md`](docs/PLAN.md) "Архитектурные принципы" and the
phase specs; do not violate them without an explicit decision:

- **Provider abstraction.** All LLM access goes through the single
  `LLMProvider` interface in `backend/app/providers/base.py`. Never call an LLM
  SDK directly from the agent loop, tools, or API layer.
- **Streaming-first.** LLM calls stream tokens via SSE/WebSocket. Interactive
  runs are cancellable through the in-process `run_registry`.
- **Pluggable registries.** Tools, skills, and (future) MCP servers are
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
  and budget guards. Do not add side effects outside a run's event log.
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

- `.env` is gitignored (only `.env.example` is tracked). API keys are encrypted
  at rest via `backend/app/core/security.py`. **Never commit `.env`, `*.db`, or
  runtime data.**
- `data/`, `workspaces/`, `evals_data*/` are gitignored runtime artifacts.
- To configure locally: `cp .env.example .env` and set at least
  `OPENAI_API_KEY` (or `OPENAI_BASE_URL` for an OpenAI-compatible backend).

## `backend/app`

### Layout

```
backend/app/
├── main.py            # create_app(): routers under /api, WS at /ws; lifespan runs init_db()
├── core/              # config (pydantic-settings), db, logging, security (Fernet)
├── providers/         # LLMProvider + OpenAI/Anthropic impls, registry, resilience, pricing
├── agent/             # loop: executor, runners, service, events, runs, approvals, permissions
├── security/          # capability policy, SSRF, secrets, sandbox, breakpoints, cost
├── tools/             # tool registry + builtins (files, code, web)
├── budgets/           # spend/budget service
├── artifacts/         # content-addressed artifact storage
├── api/               # HTTP + WebSocket routes + schemas
├── models/            # SQLModel tables
├── skills/, mcp/, memory/, tasks/, telegram/, observability/  # planned phases
```

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
pytest                           # run the 285-test suite (required)
pytest tests/test_agent.py -v    # single file
python -m evals                  # agent eval CI gate: exit 0 pass / 1 regression / 2 config
pytest tests/test_evals.py -v   # evals as pytest
alembic upgrade head             # apply migrations
alembic revision --autogenerate -m "describe change"   # new migration after model edits
```

### Co-change expectations (from git history)

- `agent/executor.py` + `agent/runners.py` → update `tests/test_agent.py`.
  Every agent-loop change in history shipped with agent tests.
- `app/models/*.py` → add a `backend/alembic/versions/*.py` migration
  (autogenerate it; verify with `alembic upgrade head`).
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

React 19 + TypeScript + Vite + Tailwind 4. State via `zustand` and
`@tanstack/react-query`. UI primitives in `src/components/ui` (Radix-based).

```
frontend/src/
├── main.tsx, App.tsx        # entry + routing
├── api/                    # typed boundary to backend (types.ts, streaming.ts, client.ts, ...)
├── hooks/                  # useConversationStream.ts (SSE/WS) and others
├── components/              # chat/ (MessageBubble, ToolCallBlock, ApprovalDialog, ...) + ui/ + layout/
├── pages/                  # ChatPage.tsx, SettingsPage.tsx, ...
├── lib/                    # utils.ts, queryClient.ts
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

- `src/api/types.ts` ↔ `backend/app/api/schemas.py` — keep shapes in sync.
- `src/hooks/useConversationStream.ts` ↔ `backend/app/api/websocket.py` +
  `backend/app/agent/events.py`.
- A new backend SSE/WS event → add a handler in `useConversationStream.ts` and
  render it in `src/components/chat/*`.
- Shared UI primitives in `src/components/ui` are consumed across `chat/` —
  don't break existing consumers when editing them.

## Definition of done

Before declaring a task complete, run the commands for every root you touched:

- Touched `backend/`? From `backend/`:
  ```bash
  ruff check . && mypy app && pytest && python -m evals
  ```
- Touched `frontend/`? From `frontend/`:
  ```bash
  npm run lint && npm run build
  ```
- Touched the **API contract** (schemas/events/WebSocket)? Update **both**
  sides and re-run both root command sets.
- Touched `app/models/*.py`? Add the Alembic migration and verify
  `alembic upgrade head`.
