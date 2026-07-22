# Фаза 3b: Recurring Tasks / Cron Jobs + RSS + Webhook

> **Статус:** ⏳ Ожидает
> **Длительность:** 1.5 недели

Цель: повторяющиеся задачи по расписанию с доставкой результатов, агрегация RSS, входящие вебхуки.

---

## Требования к расписанию

- Timezone задаётся на уровне пользователя и задачи; UI показывает ближайшие запуски в локальном времени
- Помимо cron: одноразовые reminder-задачи, quiet hours, policy для misfire и дедупликация повторных уведомлений
- Доставка или tool с внешним побочным эффектом может потребовать approval даже у фоновой задачи; решение и причина сохраняются в `TaskRun`
- Inbox результатов объединяет завершённые runs, ошибки, ожидающие approvals и непрочитанные уведомления

## 1. Scheduler engine

- **APScheduler** (AsyncIOScheduler) — cron-style + interval + date-триггеры, persist в SQLAlchemy-jobstore
- Scheduler запускается как часть backend (или как отдельный worker процесс)
- Jobstore = SQLite/PostgreSQL (задачи переживают рестарт)
- Защита от параллельного запуска одной задачи (`max_instances=1`, коалес, миссфайр-полиси)

## 2. Модель задачи (`ScheduledTask`)

- `id`, `user_id`, `name`, `cron_expression` (стандартный 5-полей cron)
- `prompt` или `workflow_type` (chat / deep-research / dnd-prep / custom)
- `agent_profile_id` (какая личность выполняет)
- `tools_whitelist`, `model`
- `delivery_channels` (telegram / email / webhook / только в UI)
- `enabled`, `next_run_at`, `last_run_at`, `last_status`
- `max_cost_per_run`, `timeout` (лимиты)
- Pydantic-схема для валидации cron-выражения (через `croniter`)

## 3. Task executor

- При срабатывании → порождается **subagent** (без чата, в фоне) с заданным profile/tools
- Результат сохраняется как `TaskRun` (prompt, output, токены, цена, длительность, ошибки)
- Доставка через выбранные каналы: Telegram-бот, email, webhook, notification в Web UI
- Уведомления о старте/успехе/провале

## 4. Agent-managed scheduling

- **Schedule tool** для агента: `create_task` / `list_tasks` / `update_task` / `delete_task` / `run_now`
- Пользователь **в чате**: "Каждый понедельник в 9 утра делай мне дайджест новостей по теме X и присылай в Telegram" → агент сам создаёт задачу
- Natural-language cron parsing tool: "каждый день в 8 вечера" → `0 20 * * *`

## 5. Template-based recurring workflows

- **Daily news/research digest**
- **Weekly D&D prep** (на основе campaign state из memory)
- **Code review / cleanup**
- **Memory review** (периодический пересмотр long-term memory)
- **Health check / monitoring**

## 6. RSS / News Aggregator 🆕

- Встроенный reader RSS/Atom/Substack как источник для recurring research digest
- `rss_fetch` tool: получение и парсинг RSS-ленты
- `rss_subscribe` / `rss_unsubscribe` / `rss_list` tools для управления подписками
- Автоматическое добавление в recurring task "еженедельный дайджест"
- Категоризация и дедупликация новостей
- UI: управление RSS-подписками, просмотр последних записей

## 7. Webhook Router 🆕

- Пользователь создаёт webhook endpoint → агент обрабатывает входящие события
- Поддерживаемые источники: GitHub (PR, issues, push), Notion, Slack, собственные системы
- `POST /api/webhooks/{hook_id}` — единый entry point для внешних систем
- Маппинг события → запуск task или subagent с контекстом события
- Валидация подписи (HMAC), rate limiting, retry при ошибке обработки
- UI: управление вебхуками, просмотр истории событий, replay события

## 8. Email Integration 🆕 (низкий приоритет)

- IMAP/SMTP: чтение входящих, генерация ответов, отправка дайджестов
- Канал доставки для recurring tasks
- Email tool: `send_email`, `read_inbox`, `search_emails`

## API (`app/api/tasks.py`)

- `GET/POST /api/tasks` — список/создание
- `GET/PUT/DELETE /api/tasks/{id}`
- `POST /api/tasks/{id}/run` — ручной запуск
- `GET /api/tasks/{id}/runs` — история запусков
- `POST /api/tasks/parse-cron` — NL → cron выражение
- `GET/POST /api/rss` — управление RSS-подписками
- `GET/POST /api/webhooks` — управление вебхуками
- WebSocket: `/ws/tasks` — real-time обновления статуса

## Frontend

- Страница **Tasks**: карточки задач с next-run/статусом
- Создание/редактирование (форма с cron-picker, профилем, каналами доставки, лимитами)
- **Cron-builder UI** — визуальный конструктор + human-readable preview
- История запусков каждой задачи
- "Run now", toggle enable/disable
- **Notification center** в шапке (badge с непрочитанными)
- Виджет "Scheduled tasks activity" на дашборде observability
- **RSS manager**: подписки, просмотр ленты, настройка дайджеста
- **Webhook manager**:创建/редактирование вебхуков, лог событий, replay
- **Inbox**: единая лента результатов задач, ошибок, pending approvals

## Telegram

- Команды: `/tasks`, `/task new`, `/task run <id>`
- Уведомления о завершении задач + кнопка "Открыть в Web App"
- `/rss` — управление RSS подписками

## Деливерабл

Пользователь (и сам агент в чате) может создавать повторяющиеся задачи с доставкой; RSS-агрегатор для дайджестов; вебхуки для интеграции с внешними системами.
