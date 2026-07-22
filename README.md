# Cool AI Harness

Personal AI agent harness with provider abstraction, tools, skills, MCP, subagents, long-term memory, multi-personality agents, observability, and recurring tasks. Control via web UI and Telegram (Bot + Web App).

> Status: **Фаза 0 (foundation)** — see [`docs/PLAN.md`](docs/PLAN.md) for the full roadmap.

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

## Project layout

```
cool-ai-harness/
├── backend/
│   ├── app/
│   │   ├── main.py              # FastAPI entrypoint
│   │   ├── core/                # config, logging, db, security
│   │   ├── providers/           # LLM provider abstraction (OpenAI-compatible)
│   │   ├── agent/               # agent loop + subagents (planned)
│   │   ├── tools/               # tool registry + builtins (planned)
│   │   ├── skills/              # skill registry + builtins (planned)
│   │   ├── mcp/                 # MCP client (planned)
│   │   ├── memory/              # long-term memory (planned, Фаза 3a)
│   │   ├── tasks/               # cron jobs / scheduler (planned, Фаза 3b)
│   │   ├── api/                 # HTTP/WebSocket routes
│   │   ├── telegram/            # bot + web app (planned, Фаза 5)
│   │   ├── models/              # SQLModel tables
│   │   └── observability/       # analytics (planned, Фаза 3a)
│   ├── tests/
│   └── pyproject.toml
├── frontend/                    # React SPA (planned)
├── docs/
│   └── PLAN.md                  # full roadmap
└── .env.example
```

## Roadmap

See [`docs/PLAN.md`](docs/PLAN.md) for the full plan:

- **Фаза 0** — Foundation *(current)*
- **Фаза 1** — Agent loop + tools + chat MVP
- **Фаза 1.5** — Надёжность запусков, безопасность, артефакты и evals
- **Фаза 2** — Skills + MCP + subagents
- **Фаза 3a** — Long-term memory + personalities + observability
- **Фаза 3b** — Recurring tasks / cron jobs ⏰
- **Фаза 4** — Deep research + D&D + code workflows
- **Фаза 5** — Telegram (Bot + Web App)
- **Фаза 6** — Product readiness

## License

MIT © Danil Kondratiuk
