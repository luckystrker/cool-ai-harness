# Фаза 0: Фундамент проекта

> **Статус:** ✅ Завершено
> **Длительность:** 2-3 дня

Заложить инфраструктуру и скелет проекта.

## Backend (Python)

- `pyproject.toml`: FastAPI, Uvicorn, Pydantic v2, SQLModel, httpx, python-telegram-bot, structlog, APScheduler, cryptography, croniter
- Структура монорепо:

  ```
  cool-ai-harness/
  ├── backend/
  │   ├── app/
  │   │   ├── main.py              # FastAPI app
  │   │   ├── core/                # config, logging, security
  │   │   ├── providers/           # LLM провайдеры
  │   │   ├── agent/               # agent loop, executor
  │   │   ├── tools/               # встроенные tools
  │   │   ├── skills/              # встроенные skills
  │   │   ├── mcp/                 # MCP клиент
  │   │   ├── memory/              # long-term memory
  │   │   ├── tasks/               # background jobs + scheduler
  │   │   ├── api/                 # HTTP/WebSocket routes
  │   │   ├── telegram/            # bot + web app handlers
  │   │   ├── models/              # SQLModel таблицы
  │   │   └── observability/       # метрики, трейсинг
  │   ├── tests/
  │   ├── alembic/
  │   └── pyproject.toml
  ├── frontend/                    # React SPA (Vite + TypeScript)
  ├── docker-compose.yml
  ├── .env.example
  └── README.md
  ```

- Config через pydantic-settings (читает `.env`)
- Базовый `docker-compose.yml` (backend, frontend, опц. redis/qdrant)
- `structlog` для структурированного логирования
- Базовые тесты (pytest), pre-commit (ruff, mypy, pytest)

## Frontend (React)

- Vite + React + TypeScript + Tailwind + shadcn/ui
- TanStack Query для серверного состояния, Zustand для UI state
- Скелет: layout, страница чата (пустая), страница настроек

## Деливерабл

Запускается `docker-compose up`, открывается пустой чат-интерфейс, backend отвечает health-check'ом.
