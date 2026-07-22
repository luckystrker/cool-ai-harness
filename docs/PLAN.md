# Cool AI Harness — План разработки

Полный roadmap от пустого репозитория до полноценного AI-агентского harness'а.

**Стек:** Python (FastAPI) + React SPA + SQLite + Telegram (Bot + Web App).

## Статус

| Фаза | Статус |
|------|--------|
| [Фаза 0 — Фундамент](phases/phase-0.md) | ✅ Завершено |
| [Фаза 1 — MVP (Agent Loop + Чат)](phases/phase-1.md) | ✅ **MVP готов** |
| [Фаза 1.5 — Надёжность, безопасность, артефакты](phases/phase-1.5.md) | 🔄 **Текущая** |
| [Фаза 2 — Skills + MCP + Subagents + Planning](phases/phase-2.md) | ⏳ Ожидает |
| [Фаза 3a — Memory + Personalities + Observability](phases/phase-3a.md) | ⏳ Ожидает |
| [Фаза 3b — Recurring Tasks + RSS + Webhook](phases/phase-3b.md) | ⏳ Ожидает |
| [Фаза 4 — Workflows + Multimodal + Browser/Code tools](phases/phase-4.md) | ⏳ Ожидает |
| [Фаза 5 — Telegram + Voice](phases/phase-5.md) | ⏳ Ожидает |
| [Фаза 6 — Product Readiness + Backlog](phases/phase-6.md) | ⏳ Ожидает |
| [Фаза 7 — UX Polish + DevX](phases/phase-7.md) | ⏳ Ожидает |

## Цель проекта

AI-агентский harness, который:

- Подключается к LLM через **API ключи** и через **подписочные сервисы** (Claude Pro/Max, ChatGPT Plus, Google AI Ultra)
- В комплекте: **agent loop**, **tools**, **skills**, **MCP**, **subagents**
- Управление через **веб-интерфейс** и через **Telegram** (Bot + Web App)
- Уникальные фичи: **long-term memory**, **multi-personality agents**, **observability/analytics**, **recurring tasks (cron jobs)**
- Специализированные workflows: **deep research**, **D&D сюжеты**, **кодовые задачи**, **мультимодальный анализ**

## Архитектурные принципы

- **Абстракция провайдеров** — единый `LLMProvider` интерфейс
- **Multi-user готовность** — таблицы с `user_id`, изоляция сессий
- **Pluggable архитектура** — tools, skills, MCP-серверы как реестры/плагины
- **Streaming-first** — все LLM-вызовы через стриминг токенов (SSE/WebSocket)
- **Аудит/observability** — лог каждого tool call и LLM-запроса
- **Background task ready** — отложенные/повторяющиеся задачи с первого дня
- **Безопасность как capability-модель** — права на чтение/запись/сеть/git/отправку
- **Durable execution** — каждый запуск имеет ID, журнал, отмену, лимиты
- **Provenance и контроль данных** — источник, дата, уверенность, область видимости
- **Quality gate для агентов** — evals при изменениях промптов и tools

## Порядок разработки

1. **Фаза 0** — каркас проекта ✅
2. **Фаза 1** — первый рабочий MVP: чат + tools + одна модель ✅
3. **Фаза 1.5** 🔄 — durable runs + capability security + artifacts + evals + HITL
4. **Фаза 2** — skills + MCP + subagents + planning mode
5. **Фаза 3a** — memory + personalities + analytics + knowledge base
6. **Фаза 3b** — recurring tasks / cron jobs + RSS + webhook
7. **Фаза 4** — deep research + D&D + code + multimodal + browser automation
8. **Фаза 5** — Telegram + voice interface
9. **Фаза 6** — product readiness + backlog (document AI, data analysis, export)
10. **Фаза 7** — UX polish (command palette, split view, prompt playground)

> **Итого на MVP:** ~2-3 недели → готово.
> **Безопасный foundation после Фазы 1.5:** ~3-4 недели.
> **Полная функциональность:** ~14-18 недель.
