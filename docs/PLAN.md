# 🤖 Cool AI Harness — План разработки

Полный roadmap от пустого репозитория до полноценного AI-агентского harness'а.

**Стек:** Python (FastAPI) + React SPA + SQLite + Telegram (Bot + Web App).

---

## 🎯 Цель проекта

AI-агентский harness, который:

- Подключается к LLM через **API ключи** и через **подписочные сервисы** (Claude Pro/Max, ChatGPT Plus, Google AI Ultra)
- В комплекте: **agent loop**, **tools**, **skills**, **MCP**, **subagents**
- Управление через **веб-интерфейс** и через **Telegram** (Bot + Web App)
- Уникальные фичи: **long-term memory**, **multi-personality agents**, **observability/analytics**, **recurring tasks (cron jobs)**
- Специализированные workflows под use-cases автора: **deep research по интернету**, **сюжеты для D&D**, **кодовые задачи**

**Траектория:** сначала личный инструмент, архитектурно заложить рост в публичный продукт.

---

## 📐 Архитектурные принципы (заложить с первого дня)

- **Абстракция провайдеров** — единый `LLMProvider` интерфейс; менять OpenAI/Claude/local без переделки кода
- **Multi-user готовность** — таблицы с `user_id`, изоляция сессий, даже если сейчас один пользователь
- **Pluggable архитектура** — tools, skills, MCP-серверы как реестры/плагины, а не зашитый код
- **Streaming-first** — все LLM-вызовы через стриминг токенов (SSE/WebSocket)
- **Аудит/observability** — лог каждого tool call и LLM-запроса с метаданными (модель, токены, цена, latency)
- **Background task ready** — архитектура с самого начала поддерживает отложенные/повторяющиеся задачи
- **Безопасность как capability-модель** — права выдаются не только на tool целиком, но и на конкретные возможности: чтение/запись в разрешённых workspace, shell, сеть, git и внешнюю отправку данных. Опасные действия по умолчанию требуют явного подтверждения.
- **Durable execution** — у каждого агентского запуска есть идентификатор, журнал событий, отмена, лимиты и возможность безопасно пережить отключение клиента или рестарт процесса.
- **Provenance и контроль данных** — факты в памяти, результаты research и артефакты хранят источник, дату, уверенность и область видимости; пользователь может проверить, исправить, экспортировать или удалить свои данные.
- **Quality gate для агентов** — изменения промптов, tools и provider-адаптеров проверяются сценарными evals, включая безопасность, корректный выбор tool и качество ответов.

---

## 🗺️ Roadmap по фазам

### Фаза 0: Фундамент проекта (2-3 дня)

Заложить инфраструктуру и скелет.

**Backend (Python):**

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
  │   │   ├── tasks/               # background jobs + scheduler (Фаза 3b)
  │   │   ├── api/                 # HTTP/WebSocket routes
  │   │   ├── telegram/            # bot + web app handlers
  │   │   ├── models/              # SQLModel таблицы
  │   │   └── observability/       # метрики, трейсинг
  │   ├── tests/
  │   ├── alembic/                 # миграции
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

**Frontend (React):**

- Vite + React + TypeScript + Tailwind + shadcn/ui
- TanStack Query для серверного состояния, Zustand для UI state
- Скелет: layout, страница чата (пустая), страница настроек

**Деливерабл:** Запускается `docker-compose up`, открывается пустой чат-интерфейс, backend отвечает health-check'ом.

---

### Фаза 1: MVP — Agent Loop + одна модель + чат (1-1.5 недели)

Цель: рабочий чат с агентом, который умеет вызывать инструменты.

**Backend:**

1. **Provider abstraction** (`app/providers/`)
   - Абстрактный `LLMProvider`: `chat_completion(messages, tools, stream)`, `embed(text)`
   - `OpenAIProvider` (работает с OpenAI, OpenRouter, DeepSeek, Groq, локальными через Ollama)
   - `AnthropicProvider` (Claude через API)
   - `ProviderRegistry` — выбирает провайдера по конфигу/модели
   - **Подписочные сервисы** (`providers/subscription/`) — адаптеры для Claude Pro/Max, ChatGPT Plus (experimental)
   - Унифицированная схема `messages`, `tools`, `responses`

2. **Agent loop** (`app/agent/`)
   - `AgentExecutor`: LLM → tool_calls → execute → повторить → финальный ответ
   - `max_iterations`, `max_tokens`, `max_cost` (защита от зацикливания)
   - Streaming через async generator
   - История в SQLite (Conversation → Message с role, content, tool_calls, tool_results)

3. **Базовые tools** (`app/tools/`)
   - `ToolRegistry` + декоратор `@tool` (Pydantic-схема → JSON Schema)
   - Встроенные: `web_search`, `web_fetch`, `python_execute` (sandboxed), `read_file`/`write_file`, `http_request`
   - Логирование каждого вызова

4. **API layer** (`app/api/`)
   - CRUD для conversations/messages
   - `POST /api/conversations/{id}/messages` — SSE стрим
   - `WebSocket /ws/chat/{conversation_id}` — real-time стрим токенов + tool events
   - `GET/POST /api/settings/providers` — управление ключами (шифрование)

5. **БД модели** (`app/models/`)
   - `User`, `Conversation`, `Message`, `ToolCall`, `ToolResult`, `Provider`
   - Alembic миграции

**Frontend:**

- Чат-интерфейс со стримингом токенов
- Отображение tool calls (collapsible)
- Markdown + подсветка кода (react-markdown + rehype-highlight)
- Sidebar с беседами
- Страница настроек провайдеров

**Деливерабл:** Рабочий чат с агентом, tool-calling, стриминг, история сохраняется.

---

### Фаза 1.5: Надёжность, безопасность и артефакты (1 неделя)

Цель: до подключения внешних MCP-серверов, subagents и cron-задач сделать каждый запуск агента управляемым, проверяемым и безопасным.

**1. Durable agent runs** (`app/agent/runs/`):

- `AgentRun` + append-only event log: состояние запуска, шаги LLM/tool, стоимость, ошибка и итоговый артефакт
- Отмена, таймаут, ограничение числа итераций и бюджета; корректная остановка дочерних задач
- Checkpoint после каждого tool call; reconnect/replay стрима без потери прогресса
- Для фоновых работ: idempotency key, retry-policy с backoff и защита от повторного выполнения

**2. Capability security** (`app/security/`, `app/tools/`):

- Разделить разрешения на `read`, `write`, `execute`, `network`, `git`, `send_external`; scopes включают workspace и домены
- Файловые tools работают только в разрешённых workspace; сетевые tools используют allowlist, ограничения размера/времени и защиту от SSRF
- Sandbox для выполнения кода без доступа к host secrets; секреты маскируются в сообщениях, трассах и логах
- UI показывает параметры опасного действия, diff/preview и историю approval; approval имеет срок действия и аудит

**3. Артефакты и вложения** (`app/artifacts/`):

- Унифицированный `Artifact`: файлы, изображения, документы, код, research-отчёты и результаты tool calls
- Upload, безопасное хранение, предпросмотр, версия/родительский run, скачивание и экспорт
- Контекст из вложений (PDF, изображение, текст, аудио после транскрибации) с лимитами и ссылкой на первоисточник

**4. Agent evals** (`backend/evals/`):

- Набор сценариев для tool selection, отказа от небезопасных действий, корректности цитат и лимитов стоимости
- Replay сохранённых трасс и сравнение модели/промпта по качеству, latency и цене
- CI quality gate: изменение агента не принимается при регрессии критичных сценариев

**Деливерабл:** агентский запуск можно отменить и восстановить; опасные возможности изолированы и подтверждаемы; файлы становятся полноценными результатами работы; ключевые сценарии защищены evals.

---

### Фаза 2: Skills + MCP + Subagents (1.5-2 недели)

**Жизненный цикл плагинов и MCP:**

- Manifest с версией, совместимостью, автором, требуемыми capabilities и схемой конфигурации
- Установка с pinning версий, health-check, явное включение/отключение и безопасный rollback
- MCP-сервер получает только выданные ему secrets и scopes; tool description и разрешения отображаются в UI
- Subagent наследует минимальный набор capabilities, отдельный бюджет и cancellation token, а не полный доступ родительского агента

**Skills система** (`app/skills/`):

- Концепция как у ZCode: `Skill` = директория с `SKILL.md` + опц. скрипты/ресурсы
- `SkillRegistry` загружает из builtin + user + плагинов
- Автоматическое определение релевантного skill (описание + embedding similarity)
- Встроенные: `deep-research`, `dnd-story`, `code-task`, `summarize-document`, `translate`, `brainstorm`

**MCP клиент** (`app/mcp/`):

- MCP-клиент (stdio + HTTP), инструменты автоматически регистрируются в `ToolRegistry`
- Конфиг через `config.yaml` или UI

**Subagents** (`app/agent/subagents/`):

- Агент порождает подзадачу → новый `AgentExecutor` со своим контекстом/tools/skills/моделью
- Параллельное выполнение, `Task` tool: `spawn_subagent(prompt, tools, model) → result`

**Frontend:**

- UI для skills и MCP-серверов
- Отображение subagent-активности в чате

**Деливерабл:** Авто-активация skills, внешние MCP-инструменты, subagents для сложных задач.

---

### Фаза 3a: Уникальные фичи — Memory + Personalities + Observability (2 недели)

**Сквозные требования к памяти и аналитике:**

- Каждая запись памяти имеет source/provenance, дату, confidence, namespace, TTL и статус подтверждения пользователем
- В UI: «почему это запомнено?», редактирование, закрепление, forget, экспорт и удаление всех данных пользователя
- Извлечение памяти дедуплицируется и не перезаписывает подтверждённые пользователем факты без явного согласия
- Трасса run связывает LLM-вызовы, tool calls, subagents, approvals и артефакты в единое дерево; её можно открыть и воспроизвести

**1. Long-term memory** (`app/memory/`):

- **Working memory** — контекст беседы (auto-summarization при превышении лимита)
- **Episodic memory** — все значимые взаимодействия с embeddings, semantic search
- **Semantic memory** — факты о пользователе (авто-извлечение)
- **Entity memory** — именованные сущности с атрибутами и связями
- Хранилище: SQLite + sqlite-vec (позже Qdrant)
- Memory tool для явного поиска/обновления
- UI: просмотр и редактирование памяти

**2. Multi-personality agents** (`app/agent/personalities/`):

- "Agent Profile": системный промпт, набор tools/skills, модель, настройки, свой memory namespace
- Presets: DM, Coder, Researcher, Writer, Assistant
- CRUD через UI, переключение в чате, могут вызывать друг друга как subagents

**3. Observability / Analytics** (`app/observability/`):

- Лог LLM-вызовов (модель, токены, цена, latency, provider)
- Лог tool calls (name, args, result, duration, success/error)
- Дашборд: расходы, топ tools, latency, история вызовов
- OpenTelemetry-экспорт (опц.)

**Деливерабл:** Память между сессиями, разные "личности", полная аналитика.

---

### Фаза 3b: Recurring Tasks / Cron Jobs ⏰ (1 неделя)

**Требования к расписанию, ориентированному на пользователя:**

- Timezone задаётся на уровне пользователя и задачи; UI показывает ближайшие запуски в локальном времени
- Помимо cron: одноразовые reminder-задачи, quiet hours, policy для misfire и дедупликация повторных уведомлений
- Доставка или tool с внешним побочным эффектом может потребовать approval даже у фоновой задачи; решение и причина сохраняются в `TaskRun`
- Inbox результатов объединяет завершённые runs, ошибки, ожидающие approvals и непрочитанные уведомления, не смешивая их с чатами

**Концепция:** Пользователь задаёт агенту задачу, которая должна выполняться по расписанию (cron). Хэрнесс сам запускает её в фоне и доставляет результат.

**Backend** (`app/tasks/`):

1. **Scheduler engine**
   - **APScheduler** (AsyncIOScheduler) — cron-style + interval + date-триггеры, persist в SQLAlchemy-jobstore
   - Scheduler запускается как часть backend (или как отдельный worker процесс для продукта)
   - Jobstore = SQLite/PostgreSQL (задачи переживают рестарт)
   - Защита от параллельного запуска одной задачи (`max_instances=1`, коалес, миссфайр-полиси)

2. **Модель задачи** (`ScheduledTask`):
   - `id`, `user_id`, `name`, `cron_expression` (стандартный 5-полей cron)
   - `prompt` или `workflow_type` (chat / deep-research / dnd-prep / custom)
   - `agent_profile_id` (какая личность выполняет)
   - `tools_whitelist`, `model`
   - `delivery_channels` (telegram / email / webhook / только в UI)
   - `enabled`, `next_run_at`, `last_run_at`, `last_status`
   - `max_cost_per_run`, `timeout` (лимиты)
   - Pydantic-схема для валидации cron-выражения (через `croniter`)

3. **Task executor**:
   - При срабатывании → порождается **subagent** (без чата, в фоне) с заданным profile/tools
   - Результат сохраняется как `TaskRun` (prompt, output, токены, цена, длительность, ошибки)
   - Доставка через выбранные каналы: Telegram-бот, email, webhook, notification в Web UI
   - Уведомления о старте/успехе/провале

4. **Agent-managed scheduling** (уникальная фича):
   - **Schedule tool** для агента: `create_task` / `list_tasks` / `update_task` / `delete_task` / `run_now`
   - Пользователь **в чате**: "Каждый понедельник в 9 утра делай мне дайджест новостей по теме X и присылай в Telegram" → агент сам создаёт задачу
   - Natural-language cron parsing tool: "каждый день в 8 вечера" → `0 20 * * *`

5. **Template-based recurring workflows** — встроенные шаблоны:
   - **Daily news/research digest**
   - **Weekly D&D prep** (на основе campaign state из memory)
   - **Code review / cleanup**
   - **Memory review** (периодический пересмотр long-term memory)
   - **Health check / monitoring**

**API** (`app/api/tasks.py`):

- `GET/POST /api/tasks` — список/создание
- `GET/PUT/DELETE /api/tasks/{id}`
- `POST /api/tasks/{id}/run` — ручной запуск
- `GET /api/tasks/{id}/runs` — история запусков
- `POST /api/tasks/parse-cron` — NL → cron выражение
- WebSocket: `/ws/tasks` — real-time обновления статуса

**Frontend:**

- Страница **Tasks**: карточки задач с next-run/статусом
- Создание/редактирование (форма с cron-picker, профилем, каналами доставки, лимитами)
- **Cron-builder UI** — визуальный конструктор + human-readable preview
- История запусков каждой задачи
- "Run now", toggle enable/disable
- **Notification center** в шапке (badge с непрочитанными)
- Виджет "Scheduled tasks activity" на дашборде observability

**Telegram:**

- Команды: `/tasks`, `/task new`, `/task run <id>`
- Уведомления о завершении задач + кнопка "Открыть в Web App"

**Деливерабл:** Пользователь (и сам агент в чате) может создавать повторяющиеся задачи, которые выполняются по расписанию и доставляют результат через web/telegram.

---

### Фаза 4: Специализированные workflows (1.5-2 недели)

**Общие требования к специализированным workflows:**

- Результат — это `Artifact`, связанный с исходными файлами, run и используемой версией skill
- У research-утверждений есть кликабельные цитаты, дата получения, сохранённый фрагмент источника и пометка об уверенности/конфликте источников
- Пользователь может возобновить или повторить workflow с теми же входными данными и сравнить результаты разных моделей

**1. Deep Research skill** (`app/skills/builtin/deep-research/`):

- Декомпозиция запроса → параллельный поиск (subagents) → fetch/extract/оценка → синтез с цитированием
- Структурированный отчёт + библиография
- Сохранение в library, экспорт в PDF/DOCX
- **Связь с Фазой 3b:** можно запускать как recurring task (еженедельный дайджест по теме)

**2. D&D Story Crafter skill** (`app/skills/builtin/dnd-story/`):

- DM-агент со знанием 5e, хранение campaign state (мир/NPC/локации/квесты) в memory
- Генерация one-shot'ов, кампаний, NPC, энкаунтеров (с балансом по CR), лора
- Dice rolling tool
- Экспорт в PDF
- **Связь с Фазой 3b:** weekly prep recurring task

**3. Code Task skill** (`app/skills/builtin/code-task/`):

- Агент-кодер с sandboxed Python/Bash, persistent workspace с git
- Tools: `python_execute`, `bash_execute`, `read_file`, `write_file`, `git`, `web_search`
- Мульти-файловые проекты, автотесты
- **Связь с Фазой 3b:** long-running фоновые задачи — уведомление в telegram по завершении

**Frontend:** спец-страницы для каждого workflow (deep research с прогресс-баром, D&D менеджер кампаний, code IDE-подобный вид).

**Деливерабл:** Три мощных специализированных инструмента под use-cases автора.

---

### Фаза 5: Telegram (Bot + Web App) (1 неделя)

**Telegram Bot** (`app/telegram/bot/`):

- Через `python-telegram-bot` (async)
- Команды: `/start`, `/chat`, `/profile`, `/research`, `/dnd`, `/code`, `/tasks`, `/settings`
- Inline-кнопки (профили, модели, действия по задачам)
- Streaming через edit_message
- Уведомления о завершении long-running и recurring tasks
- Опц.: голосовые → Whisper → обработка

**Telegram Web App** (`app/telegram/webapp/`):

- React-приложение внутри Telegram, авторизация через `initData`
- Полный UI: чат, профили, memory, аналитика, **tasks**
- Нативные фичи: камера (multimodal), файлы, геолокация
- Темизация под Telegram

**Общий auth:** один пользователь, единый токен между web/telegram.

**Деливерабл:** Полный контроль harness'а с телефона, включая управление recurring tasks.

---

### Фаза 6: Product readiness (по мере роста)

- **Auth:** OAuth, JWT, multi-tenant
- **Биллинг:** Stripe, квоты, тарифы
- **Надёжность и данные:** backup/restore для БД и артефактов, экспорт/импорт пользовательских данных, disaster-recovery runbook и регулярная проверка восстановления
- **Provider resilience:** capability discovery, fallback-модели, retries с учётом rate limits, circuit breaker и дневные/месячные бюджеты
- **Масштабирование:** SQLite → PostgreSQL, Celery/RQ + Redis для фоновых задач, горизонтальное масштабирование
- **Отдельный worker процесс** для scheduler (multi-instance deployments)
- **Безопасность:** RBAC, аудит, шифрование secrets, ротация ключей, secret-scoping и политика retention для трасс/артефактов
- **Rate limiting, anti-abuse**
- **Plugin marketplace** (публичные skills/MCP/персональности/tasks-templates) с проверкой manifest, версий, подписей/доверенного издателя и permissions review

---

## 🛠️ Технический стек — финальный

| Слой | Технология |
|------|-----------|
| Backend | Python 3.12+, FastAPI, Uvicorn |
| Scheduler | **APScheduler** (cron jobs) + croniter (NL/валидация) |
| ORM/БД | SQLModel + SQLite (→ PostgreSQL) |
| Векторный поиск | sqlite-vec (MVP) → Qdrant (продукт) |
| LLM SDK | httpx + собственная абстракция |
| Tools sandbox | Docker containers |
| Фронтенд | React 18 + TypeScript + Vite + Tailwind + shadcn/ui |
| Состояние | TanStack Query + Zustand |
| Telegram | python-telegram-bot v21 |
| Логи/метрики | structlog + OpenTelemetry (опц.) |
| Контейнеризация | Docker + docker-compose |
| Тесты | pytest + Vitest |
| CI | GitHub Actions |

---

## 📦 Порядок разработки

1. **Фаза 0** (3 дня) → каркас
2. **Фаза 1** (1-1.5 нед) → **первый рабочий MVP**: чат + tools + одна модель
3. *Контрольная точка:* harness уже полезен
4. **Фаза 1.5** (1 нед) → durable runs + capability security + artifacts + evals
5. **Фаза 2** (1.5-2 нед) → skills + MCP + subagents
6. **Фаза 3a** (2 нед) → memory + personalities + analytics
7. **Фаза 3b** (1 нед) → **recurring tasks / cron jobs** ⏰
8. **Фаза 4** (1.5-2 нед) → deep research + D&D + code
9. **Фаза 5** (1 нед) → Telegram
10. **Фаза 6** → по мере необходимости

> **Примечание по Фазе 3b:** рекуррентные задачи требуют subagents (Фаза 2) и personalities (Фаза 3a) для выбора профиля, поэтому логично идут после них. Но архитектурный фундамент (APScheduler в `pyproject.toml`, `tasks/` модуль, поле `user_id` в моделях) закладывается уже в Фазе 0.

**Итого на MVP: ~2-3 недели** → рабочий harness.
**Безопасный и расширяемый foundation после Фазы 1.5: ~3-4 недели.**
**Полная функциональность со всеми фичами (включая cron jobs): ~11-14 недель.**
