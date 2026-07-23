# Фаза 1.5: Надёжность, безопасность и артефакты

> **Статус:** 🔄 Текущая
> **Длительность:** 1 неделя

Цель: до подключения внешних MCP-серверов, subagents и cron-задач сделать каждый запуск агента управляемым, проверяемым и безопасным.

---

## 1. Durable agent runs ✅

**Реализовано:** `AgentRun` + append-only event log (`run_events`), статус запуска
(running/awaiting_approval/completed/failed/cancelled), кумулятивный usage,
checkpoint после каждого tool call, отмена через `run_registry` (in-process),
cost/token budget guards, конфигурируемые лимиты (`agent_max_iterations`,
`agent_max_total_tokens`, `agent_max_cost_usd`, `agent_run_timeout_s`).

API: `GET /conversations/{id}/runs`, `GET .../runs/{id}` (детали + event log),
`GET .../runs/{id}/events`, `POST .../runs/{id}/cancel`. Миграции — через
Alembic (`backend/alembic`), baseline + `agent_runs`/`run_events`.

**Что отложено** (к Разделу 6 — Inspector/Replay): полная реконструкция
оборванной mid-LLM-stream итерации и рестарт процесса с продолжением;
idempotency key / retry-policy для фоновых работ (когда появятся cron-задачи,
Фаза 3b). Текущий checkpoint фиксирует последний завершённый шаг и event log
сохраняет каждое событие — этого достаточно для replay и будущего resume.

---

<details>
<summary>Исходная спецификация раздела</summary>

- `AgentRun` + append-only event log: состояние запуска, шаги LLM/tool, стоимость, ошибка и итоговый артефакт
- Отмена, таймаут, ограничение числа итераций и бюджета; корректная остановка дочерних задач
- Checkpoint после каждого tool call; reconnect/replay стрима без потери прогресса
- Для фоновых работ: idempotency key, retry-policy с backoff и защита от повторного выполнения

</details>


## 2. Capability security (`app/security/`, `app/tools/`)

- Разделить разрешения на `read`, `write`, `execute`, `network`, `git`, `send_external`; scopes включают workspace и домены
- Файловые tools работают только в разрешённых workspace; сетевые tools используют allowlist, ограничения размера/времени и защиту от SSRF
- Sandbox для выполнения кода без доступа к host secrets; секреты маскируются в сообщениях, трассах и логах
- UI показывает параметры опасного действия, diff/preview и историю approval; approval имеет срок действия и аудит

### Human-in-the-Loop Breakpoints 🆕

- Возможность вставить точку остановки в цепочку tool calls: агент выполняет шаги до breakpoint, затем ждёт явного подтверждения от пользователя
- Поддерживаемые типы: `before_tool`, `after_tool_result`, `before_send`, `before_write`
- В UI: breakpoint отображается как блокирующий диалог с контекстом (какой tool, с какими аргументами, что будет дальше)
- В Telegram: inline-кнопки Approve / Reject / Edit args
- Пользователь может динамически поставить breakpoint на любой tool в настройках
- Breakpoints имеют TTL и fallback-действие (отклонить/пропустить) при недоступности пользователя

## 3. Артефакты и вложения (`app/artifacts/`) ✅

**Реализовано:** Унифицированный `Artifact` (модель + content-addressed хранилище):
файлы, изображения, документы, код, research-отчёты, аудио и результаты tool calls.
Upload через multipart, безопасное хранение (SHA-256 дедупликация, ограничение
размера), предпросмотр (extracted_text для текстовых файлов), версия/родительский
run, скачивание и экспорт. Kind-классификация по расширению/MIME. Soft-delete.

API: `POST /conversations/{id}/artifacts` (upload), `GET .../artifacts` (list с
фильтрами run_id/kind), `GET .../artifacts/{id}` (детали + extracted_text + versions),
`GET .../artifacts/{id}/download`, `DELETE .../artifacts/{id}`. Миграция — Alembic
`0004_artifacts`.

**Что отложено**: извлечение текста из PDF (нужна внешняя библиотека), транскрибация
аудио, генерация превью изображений — по мере подключения соответствующих
инструментов в Фазах 2–3.

## 4. Agent evals (`backend/evals/`) ✅

**Реализовано:** Фреймворк сценарных eval'ов для агентского цикла:
- `EvalScenario` — декларативное описание сценария (ввод, скрипт LLM, ассерты, теги, severity)
- `EvalRunner` — выполняет сценарии через `AgentExecutor`, собирает метрики (latency, tokens, iterations), проверяет ассерты
- 21 встроенный сценарий: tool selection (8), safety/capability denial (8), cost/iteration limits (5)
- Replay & comparison: `TraceStore` сохраняет baselines, `compare_runs()` строит `ComparisonReport` с регрессиями/фиксами/метриками
- CI quality gate: `python -m evals` — exit code 0/1/2, фильтр по тегам, сравнение с baseline, verbose-режим
- Pytest-интеграция: `tests/test_evals.py` (20 тестов) прогоняет все сценарии и проверяет replay/gate логику

**Что отложено**: live-провайдер evals (реальные LLM-вызовы для A/B тестирования промптов) — по мере
подключения Provider Resilience (§5) и Inspector (§6).

## 5. Provider Resilience & Cost Guards (`app/providers/`, `app/security/`)

- **Retry с Backoff** 🆕: автоматический retry LLM-вызовов при rate limit / timeout с exponential backoff, jitter; fallback на резервную модель при недоступности основной
- **Circuit breaker** для провайдеров: переход на fallback при серии ошибок
- **Cost Budget Alerts** 🆕: дневные/недельные/месячные бюджеты; алерт при достижении 80 %; блокировка при превышении (с возможностью явного override)
- UI: страница бюджетов с историей расходов в реальном времени

## 6. Debug / Inspector Mode (`app/observability/inspector/`) 🆕

- "Developer Tools" для агентских запусков: полный raw JSON каждого LLM-запроса, token allocation, timing каждого шага, diff промптов между итерациями
- Replay режим: воспроизвести run с теми же входными данными, но другой моделью/промптом
- Side-by-side сравнение двух запусков
- WebSocket для live-инспекции текущего запуска

---

## Деливерабл

Агентский запуск можно отменить и восстановить; опасные возможности изолированы и подтверждаемы с breakpoint'ами; файлы — полноценные результаты; ключевые сценарии защищены evals; провайдеры resilience с retry/fallback/cost guards; полный debug-интерфейс для разработчика.
