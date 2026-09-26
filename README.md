# Cool

Personal AI agent harness with provider abstraction, tools, skills, MCP,
subagents, long-term + working memory, personalities, planning mode, recurring
tasks (cron), RSS aggregation, webhooks, wiki, cost budgets, analytics, an
inspector/replay console, and durable agent runs. Control via the web UI.

> Status: **Фазы 0–4 shipped**. Phase 4 delivers Deep Research, Code/Git/GitHub,
> multimodal chat and OCR, browser automation, and Agent Constructor ✅ —
> see [`docs/PLAN.md`](docs/PLAN.md) for the full roadmap.
<img width="1718" height="1273" alt="image" src="https://github.com/user-attachments/assets/473ff4c8-052a-4e62-a3b5-3d9a99610686" />

## Stack

- **Runtime (default):** Rust trusted core — the single `cool` binary serves
  the SPA, the canonical App Protocol (`POST /api/rpc` + `GET /api/events`),
  blob transports, the agent loop, tools, subagents and deep research.
- **Legacy lane (optional, not on the default startup path):** Python 3.12+,
  FastAPI, Uvicorn, SQLModel + SQLite, Alembic under `backend/` — kept until
  [ADR-0003](docs/migration/adr/0003-remove-legacy-python-server.md) executes;
  optional Python workers follow
  [`docs/backlog/python-workers.md`](docs/backlog/python-workers.md)
- **Frontend:** React 19 + TypeScript + Vite 8 + Tailwind 4 (zustand,
  @tanstack/react-query, Radix-based UI primitives)
- **LLM providers:** OpenAI + Anthropic via a single `LLMProvider` interface
  (OpenAI-compatible base URL works for OpenRouter/DeepSeek/Groq/Ollama)
- **Scheduler:** APScheduler (AsyncIOScheduler) + croniter — cron/interval/date
  recurring agent tasks (Фаза 3b)
- **RSS:** feedparser-based aggregator with per-subscription filters and LLM
  summarization (Фаза 3b)
- **Observability:** unified LLM-call log, aggregating dashboards, optional
  OpenTelemetry export (Фаза 3a)
- **Telegram:** python-telegram-bot dependency present (Bot + Web App — planned,
  Фаза 5)

## Quick start

### 1. Configure

```bash
cp .env.example .env
# edit .env — set at least OPENAI_API_KEY (or OPENAI_BASE_URL for an
# OpenAI-compatible backend like OpenRouter/DeepSeek/Groq/Ollama)
# also generate a SECRET_KEY (Fernet = 32 url-safe base64 bytes):
openssl rand -base64 32
# or with the Python lane: python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())"
```

### 2. Run the packaged app (recommended)

```bash
# the container binds inside its own network namespace, so it needs a token
echo "COOL_API_TOKEN=$(openssl rand -hex 24)" >> .env
docker compose up --build
```

Open http://127.0.0.1:8000. The production image is a **Rust core + React
bundle**: one `cool` binary serves the UI, the canonical App Protocol API
(`POST /api/rpc`), and the cursor/reconnect event stream (`GET /api/events`) from
one process and one port. The runtime stage contains neither Python nor Node;
Bun is never installed (the OpenCode worker is an opt-in sidecar). State persists
in the Docker-managed `cool-state` volume. The port is published on loopback
only; the `server` profile for VPS deployment requires a TLS/reverse-proxy
boundary.

The container requires a shared token for the API surface. Open the SPA once
with it — `http://127.0.0.1:8000/?token=<COOL_API_TOKEN>` — and the bundle stores
it for the tab and strips it from the URL; the static bundle is public while
`/api/rpc` and `/api/events` stay token-gated.

#### Upgrading an existing Python install

A legacy data root (`harness.db` at the Alembic baseline plus `rust-core.db`)
keeps serving read-only until an operator adopts it:

```bash
cool store adopt --data-dir /var/lib/cool   # verified backup, then Rust owns migrations
cool serve --data-dir /var/lib/cool --assets frontend/dist --legacy-store
```

In the container the image CMD already serves the legacy families
(`--legacy-store`). A fresh volume gets a baseline `harness.db`; an existing
Python-owned one stays read-only until it is adopted:

```bash
docker compose run --rm cool cool store adopt --data-dir /var/lib/cool
docker compose up
```

Adoption takes a verified backup before the first Rust write and records the
migration owner, so the pre-adoption snapshot can be restored. Re-running adopt
is idempotent; a database at another Alembic revision fails closed. Never run
Alembic and the Rust store against the same file.

### 3. Unified source install

```bash
cd frontend
npm ci
npm run build

cd ..
cargo build --release -p cool-cli
./target/release/cool serve --assets frontend/dist --legacy-store
```

This opens the complete application at http://127.0.0.1:8000 (App Protocol API +
SPA). `--legacy-store` initializes a fresh baseline store on first run, or serves
an existing Python-owned data root read-only until you adopt it (see above). For
frontend hot reload, run `cool serve` in one terminal and `npm run dev`
from `frontend/` in another; Vite remains the development-only split mode. The
legacy Python/FastAPI backend in `backend/` still exists during the migration
but is not on the default startup path and is not the production packaging.
Rolling an installation back to the Python runtime is documented in
[`docs/migration/M12_ROLLBACK.md`](docs/migration/M12_ROLLBACK.md); removing
the legacy server entirely is gated on
[ADR-0003](docs/migration/adr/0003-remove-legacy-python-server.md).

### 4. Smoke test

```bash
# health
curl http://localhost:8000/api/health

# chat (non-streaming MVP endpoint)
curl -X POST http://localhost:8000/api/chat \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"Say hello in one short sentence."}]}'
```

The stable entrypoint, path layout, Docker persistence, VPS limitations, and
future Rust replacement boundary are documented in
[`docs/migration/M2_PACKAGING_CONTRACT.md`](docs/migration/M2_PACKAGING_CONTRACT.md).
Docker Compose intentionally publishes only on loopback. The current M2 runtime
is local/single-user only; do not expose it to the Internet. `API_TOKEN` remains
a compatibility option for direct API clients, but the packaged SPA has no token
bootstrap and setting it does not turn M2 into a supported VPS deployment. The
authenticated server profile and Telegram identity adapter remain later phases.

## Durable runs & migrations

Each agent turn is a **durable run** (Фаза 1.5): an `agent_runs` row tracks its
status (`running` → `completed`/`failed`/`cancelled`), cumulative token/cost
usage, iterations, and outcome; an append-only `run_events` log records every
event for replay/inspection. Interactive runs (SSE/WebSocket) are cancellable
via the registry and the cancel endpoint.

On the default runtime these are canonical App Protocol commands
(`session.runs`, `run.events`, `run.cancel`) over `POST /api/rpc` — for
example:

```bash
curl -X POST http://localhost:8000/api/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"cool.command","params":{"protocolVersion":1,"commandId":"c1","command":{"method":"run.events","params":{"runId":"run-…","limit":50}}}}'
```

Schema changes are owned by the Rust store (`crates/cool-store`) at the frozen
Alembic baseline `0022_phase4_completion`; the legacy **Alembic** toolchain
(`backend/alembic`) only remains for the optional legacy lane and refuses a
Rust-owned database. In development/tests the Python lane still uses
`SQLModel.create_all` (models are the source of truth there).

```bash
cd backend
alembic upgrade head                          # apply migrations
alembic revision --autogenerate -m "..."      # create a new migration
```

## Agent evals (CI quality gate)

The `backend/evals/` package contains a scenario-driven evaluation framework
that verifies the agent loop's tool selection, safety policy enforcement, and
cost/iteration limits. All scenarios are **deterministic** (scripted LLM
responses) — no API keys needed.

```bash
cd backend

# Run all 23 scenarios (gate fails if any critical scenario fails)
python -m evals

# Filter by tag (repeatable)
python -m evals --tag safety
python -m evals --tag tool_selection
python -m evals --tag cost

# Verbose output (shows per-assertion details on failure)
python -m evals -v

# Save current results as a baseline for future comparison
python -m evals --update-baseline

# Compare against a saved baseline (fails on critical regressions)
python -m evals --baseline default

# Machine-readable output for CI
python -m evals --json
```

Scenarios live in four suites under `backend/evals/scenarios/`: `safety` (8),
`tool_selection` (8), `cost_limits` (5), and `research` (2).

**Exit codes:** `0` = gate passed, `1` = critical regression/failure, `2` = config error.

**Pytest integration** — evals also run as part of the test suite:

```bash
python -m pytest tests/test_evals.py -v
```

**Writing new scenarios** — add an `EvalScenario` to the appropriate suite in
`backend/evals/scenarios/` (tool_selection, safety, or cost_limits). Each
scenario declares a scripted LLM response and assertions:

```python
from evals.scenario import EvalScenario, ScenarioAssertion, Severity

EvalScenario(
    id="my_scenario",
    name="Description",
    tags=["safety"],
    severity=Severity.CRITICAL,
    input="User message",
    script=[
        [{"name": "tool_name", "arguments": {"key": "value"}}],  # LLM calls a tool
        "Final text response",                                     # LLM responds
    ],
    assertions=[
        ScenarioAssertion(type="tool_called", name="tool_name"),
        ScenarioAssertion(type="finish_reason", reason="stop"),
    ],
    config={"capability_policy": {"execute": "deny"}},  # optional overrides
)
```

## Subsystems

Beyond the core agent loop, these subsystems are implemented:

- **Durable runs** — every turn is an `AgentRun` with an append-only `run_events`
  log, status (`running`/`awaiting_approval`/`completed`/`failed`/`cancelled`),
  cumulative token/cost usage, checkpoints, and budget guards. Interactive runs
  are cancellable via the registry and the cancel endpoint.
- **Capability security** — permissions split into
  `read`/`write`/`execute`/`network`/`git`/`send_external`; file tools are
  workspace-confined, network tools use an SSRF-protected allowlist, code
  execution is sandboxed, and secrets are masked in messages/traces/logs.
- **Context management** — token-aware history budgeting/truncation, project
  instructions loading (AGENTS.md from the working directory), and working
  context compaction with collapsible chat history.
- **Cost budgets** — per-period spend limits with alert threshold and optional
  block-on-exceed; spend is logged per run.
- **Skills** — discover `SKILL.md` skills from builtin/user dirs; rank by
  TF-IDF keywords/tags (+ optional embedding similarity) and inject relevant
  skill context into the system prompt.
- **MCP** — JSON-RPC 2.0 client over stdio **and** HTTP; connects external MCP
  servers and bridges their tools into the tool registry as `mcp_{server}_{tool}`.
  A marketplace client queries `registry.modelcontextprotocol.io`.
- **Subagents** — isolated conversations + durable runs spawned from roles
  (`researcher`, `code-reviewer`, `summarizer` seeded by default); capability
  policies per role, background launch/cancel, delegated plan steps.
- **Planning mode** — research-first loop emits a fenced `plan` JSON block;
  steps have dependencies (topological execution), draft → approve → execute,
  with `plan_progress` events and templates.
- **Memory** (Фаза 3a) — long-term memory (`MemoryItem`/`Episode`) with FTS5
  recall + composite reranking, entity extraction with confirmation/explain
  panels, pinning/export, post-session LLM extraction, decay/consolidation
  sweeps, and working-memory scratchpads; project-scoped visibility. Exposed to
  the agent via memory tools.
- **Personalities** (Фаза 3a) — multiple agent profiles with distinct system
  prompts/names/descriptions; switchable per chat, persisted in the DB
  (`agent_profiles`).
- **Analytics** (Фаза 3a) — aggregating dashboards (spend, tool usage, runs,
  latency), unified LLM-call log, and optional OpenTelemetry export.
- **Inspector** — live `/ws/inspect/{run_id}` tail of in-progress runs, plus
  timeline reconstruction, two-run comparison, and replay over the event log.
- **Recurring tasks** (Фаза 3b) — APScheduler-backed cron/interval/date agent
  tasks persisted in the DB; scheduled runs are durable with delivery templates
  (reminders, reports, summaries).
- **RSS** (Фаза 3b) — feed subscriptions with filters, scheduled aggregation,
  and LLM summarization into a digest/inbox.
- **Webhooks** (Фаза 3b) — HTTP webhook router that triggers agent runs/tasks
  from external services (signed, idempotent).
- **Wiki** — markdown article store (`wiki_articles`) with agent search/write
  tools and a browsing UI.
- **Code & Git tools** (Фаза 4) — sandboxed `bash`/Python
  execution, git status/diff/log/commit/push via the local CLI, and GitHub
  integration (issues/PRs/actions).
- **Deep Research** (Фаза 4) — durable research runs with parallel subagents,
  source citations, browser activity, and Markdown/HTML/PDF/DOCX export.
- **Multimodal chat** (Фаза 4) — image/document attachments, provider-native
  vision payloads, OCR/PDF extraction, thumbnails, and analysis tools.
- **Browser automation** (Фаза 4) — isolated Playwright sessions with SSRF
  protection, navigation, interaction, extraction, and screenshot artifacts.
- **Agent Constructor** (Фаза 4) — reusable blueprints, per-agent limits,
  tool/skill selection, playground runs, sharing/cloning, and macro-tools.

## Project layout

```
cool-ai-harness/
├── backend/
│   ├── app/
│   │   ├── main.py              # FastAPI entrypoint (create_app, lifespan, routers, /ws)
│   │   ├── core/                # config (pydantic-settings), db, logging, security (Fernet)
│   │   ├── providers/           # LLMProvider + OpenAI/Anthropic impls, registry, resilience, pricing
│   │   ├── agent/               # loop: executor, runners, service, events, runs, approvals,
│   │   │                        #       permissions, planning, subagents, personalities,
│   │   │                        #       context_window, project_instructions
│   │   ├── security/            # capability policy, SSRF, secrets, sandbox, breakpoints, cost
│   │   ├── tools/               # tool registry + builtins (files, code, bash, git, github, web,
│   │   │                        #   mcp, memory, skills, plan, subagent, task, rss, wiki, context)
│   │   ├── skills/              # skill registry + discovery + TF-IDF/embedding matching
│   │   ├── mcp/                 # MCP client (stdio + HTTP), registry, marketplace, tool bridge
│   │   ├── memory/              # long-term + working memory: extractor, retrieval (FTS5),
│   │   │                        #   entities, context_builder, lifecycle, tools
│   │   ├── observability/       # run inspector: live tail, timeline, compare, replay
│   │   ├── analytics/           # aggregating dashboards, LLM-call log, OTel export
│   │   ├── budgets/             # spend/budget service
│   │   ├── artifacts/           # content-addressed artifact storage
│   │   ├── tasks/               # recurring tasks: scheduler, cron, delivery, templates (Фаза 3b)
│   │   ├── rss/                 # RSS aggregator: subscriptions, fetch, summarize (Фаза 3b)
│   │   ├── webhooks/            # webhook router (Фаза 3b)
│   │   ├── wiki/                # wiki article store + tools
│   │   ├── api/                 # HTTP + WebSocket routes + schemas
│   │   ├── models/              # SQLModel tables
│   │   └── telegram/            # bot + web app (planned, Фаза 5)
│   ├── evals/                   # agent eval scenarios + CI gate (21 scenarios)
│   ├── alembic/                 # database migrations (head: 0022_phase4_completion)
│   ├── tests/                   # pytest suite (~725 tests)
│   └── pyproject.toml
├── frontend/                    # React 19 SPA (Vite + TypeScript + Tailwind 4)
│   └── src/
│       ├── api/                 # typed boundary to backend (types, streaming, clients)
│       ├── hooks/               # useConversationStream (SSE/WS) + others
│       ├── components/          # chat/, memory/, inspector/, subagents/, layout/, settings/, ui/
│       ├── pages/               # ChatPage, MemoryPage, WikiPage, ProfilesPage, AnalyticsPage,
│       │                        #   TasksPage, SettingsPage, BudgetsPage, SubagentsPage, InspectorPage
│       └── lib/                 # utils, queryClient, agentConfig, modelFormat, projects
├── docs/
│   ├── PLAN.md                  # full roadmap
│   └── phases/                  # per-phase specs (phase-0 .. phase-7)
├── LICENSE                      # MIT
└── .env.example
```

## Roadmap

See [`docs/PLAN.md`](docs/PLAN.md) for the full plan:

| Фаза | Статус |
|------|--------|
| **Фаза 0** — Foundation | ✅ Done |
| **Фаза 1** — Agent loop + tools + chat MVP | ✅ Done |
| **Фаза 1.5** — Надёжность запусков, безопасность, артефакты, evals, HITL | ✅ Done |
| **Фаза 2** — Skills + MCP + subagents + planning mode | ✅ Done |
| **Фаза 3a** — Memory + personalities + observability | ✅ Done |
| **Фаза 3b** — Recurring tasks + RSS + webhook | ✅ Done |
| **Фаза 4** — Workflows + multimodal + browser/code tools | ✅ Done |
| **Фаза 5** — Telegram + voice interface | ⏳ |
| **Фаза 6** — Product readiness + backlog | ⏳ |
| **Фаза 7** — UX polish + DevX | ⏳ |

Each phase has its own file in [`docs/phases/`](docs/phases/).

## License

MIT © 2026 Danil Kondratiuk — see [`LICENSE`](LICENSE).
