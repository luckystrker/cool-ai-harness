# Cool AI Harness

Personal AI agent harness with provider abstraction, tools, skills, MCP, subagents, long-term memory, multi-personality agents, observability, and recurring tasks. Control via web UI and Telegram (Bot + Web App).

> Status: **Фаза 1.5 (durable runs, security, artifacts, evals)** 🔄 — see [`docs/PLAN.md`](docs/PLAN.md) for the full roadmap.

## Stack

- **Backend:** Python 3.12+, FastAPI, Uvicorn, SQLModel + SQLite
- **Frontend:** React 18 + TypeScript + Vite + Tailwind (planned)
- **Telegram:** python-telegram-bot (planned, Фаза 5)
- **Scheduler:** APScheduler + croniter (Фаза 3b)

## Quick start

### 1. Configure

```bash
cp .env.example .env
# edit .env — set at least OPENAI_API_KEY (or OPENAI_BASE_URL for an
# OpenAI-compatible backend like OpenRouter/DeepSeek/Groq/Ollama)
# also generate a SECRET_KEY:
python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())"
```

### 2. Run backend

```bash
cd backend
pip install -e ".[dev]"
uvicorn app.main:app --reload --port 8000
```

### 3. Smoke test

```bash
# health
curl http://localhost:8000/api/health

# chat (non-streaming MVP endpoint)
curl -X POST http://localhost:8000/api/chat \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"Say hello in one short sentence."}]}'
```

## Durable runs & migrations

Each agent turn is a **durable run** (Фаза 1.5): an `agent_runs` row tracks its
status (`running` → `completed`/`failed`/`cancelled`), cumulative token/cost
usage, iterations, and outcome; an append-only `run_events` log records every
event for replay/inspection. Interactive runs (SSE/WebSocket) are cancellable
via the registry and the cancel endpoint.

```bash
# List/detail a conversation's runs and their event logs
curl http://localhost:8000/api/conversations/1/runs
curl http://localhost:8000/api/conversations/1/runs/1
curl http://localhost:8000/api/conversations/1/runs/1/events

# Signal a running run to stop
curl -X POST http://localhost:8000/api/conversations/1/runs/1/cancel
```

Schema changes are managed with **Alembic** (`backend/alembic`). In production
the app applies `alembic upgrade head` on startup; in development/tests it uses
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

# Run all 21 scenarios (gate fails if any critical scenario fails)
python -m evals

# Filter by tag
python -m evals --tag safety
python -m evals --tag tool_selection
python -m evals --tag cost

# Verbose output (shows per-assertion details on failure)
python -m evals -v

# Save current results as a baseline for future comparison
python -m evals --update-baseline

# Compare against a saved baseline (fails on critical regressions)
python -m evals --baseline default
```

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

## Project layout

```
cool-ai-harness/
├── backend/
│   ├── app/
│   │   ├── main.py              # FastAPI entrypoint
│   │   ├── core/                # config, logging, db, security
│   │   ├── providers/           # LLM provider abstraction (OpenAI-compatible)
│   │   ├── agent/               # agent loop + durable runs + approvals
│   │   ├── security/            # capability policy, SSRF, secrets, sandbox, breakpoints
│   │   ├── tools/               # tool registry + builtins (files, code, web)
│   │   ├── skills/              # skill registry + builtins (planned)
│   │   ├── mcp/                 # MCP client (planned)
│   │   ├── memory/              # long-term memory (planned, Фаза 3a)
│   │   ├── tasks/               # cron jobs / scheduler (planned, Фаза 3b)
│   │   ├── api/                 # HTTP/WebSocket routes
│   │   ├── telegram/            # bot + web app (planned, Фаза 5)
│   │   ├── models/              # SQLModel tables
│   │   └── observability/       # analytics (planned, Фаза 3a)
│   ├── evals/                   # agent eval scenarios + CI gate
│   ├── alembic/                 # database migrations
│   ├── tests/
│   └── pyproject.toml
├── frontend/                    # React SPA
├── docs/
│   └── PLAN.md                  # full roadmap
└── .env.example
```

## Roadmap

See [`docs/PLAN.md`](docs/PLAN.md) for the full plan:

| Фаза | Статус |
|------|--------|
| **Фаза 0** — Foundation | ✅ Done |
| **Фаза 1** — Agent loop + tools + chat MVP | ✅ **MVP ready** |
| **Фаза 1.5** — Надёжность запусков, безопасность, артефакты, evals, HITL | 🔄 **Current** |
| **Фаза 2** — Skills + MCP + subagents + planning mode | ⏳ |
| **Фаза 3a** — Memory + personalities + observability + KB | ⏳ |
| **Фаза 3b** — Recurring tasks + RSS + webhook | ⏳ |
| **Фаза 4** — Deep research + code + multimodal + browser | ⏳ |
| **Фаза 5** — Telegram + voice interface | ⏳ |
| **Фаза 6** — Product readiness + backlog | ⏳ |
| **Фаза 7** — UX polish + DevX | ⏳ |

Each phase has its own file in [`docs/phases/`](docs/phases/).

## License

MIT © Danil Kondratiuk
