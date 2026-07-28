# Фаза 3a: Memory + Personalities + Observability + Knowledge Management

> **Статус:** 🔄 В работе (раздел 1 «Long-term memory» завершён; раздел 5 «Observability» завершён)
> **Длительность:** 2 недели

Цель: долговременная память между сессиями, разные "личности" агента, полная аналитика и система знаний.

---

## Сквозные требования к памяти и аналитике

- Каждая запись памяти имеет source/provenance, дату, confidence, namespace, TTL и статус подтверждения пользователем ✅
- В UI: «почему это запомнено?», редактирование, закрепление, forget, экспорт и удаление всех данных пользователя ✅
- Извлечение памяти дедуплицируется и не перезаписывает подтверждённые пользователем факты без явного согласия ✅
- Трасса run связывает LLM-вызовы, tool calls, subagents, approvals и артефакты в единое дерево; её можно открыть и воспроизвести

## 1. Long-term memory (`app/memory/`) — ✅ завершено

- **Working memory** — контекст беседы (auto-summarization при превышении лимита)
- **Episodic memory** — все значимые взаимодействия с embeddings, semantic search
- **Semantic memory** — факты о пользователе (авто-извлечение)
- **Entity memory** — именованные сущности с атрибутами, алиасами и связями
  (`entities`, `entity_relations`, `memory_item_entities` таблицы; LLM-извлечение
  в `memory/entities.py`; tool `entity_lookup`; секция `[RELEVANT ENTITIES]` в
  контексте)
- Хранилище: SQLite + FTS5 (позже embeddings/Qdrant)
- Memory tools для явного поиска/обновления (`memory_remember`, `memory_recall`,
  `memory_forget`, `memory_update`, `memory_list`, `set_working_memory`,
  `get_working_memory`, `entity_lookup`)
- **User-confirmation workflow**: agent-extracted памяти попадают в статус
  `pending_confirmation` и исключаются из recall/context до подтверждения
  (`confirm`/`reject`); авто-reject устаревших по TTL
- **Pin**: закрепление памяти защищает её от decay/TTL
- **Export**: JSON и Markdown выгрузка всех памятей
- **«Why remembered»**: explainability — breakdown score (importance/recency/
  confidence/type_priority), provenance и lifecycle в `memory_recall` tool и
  endpoint `GET /memory/{id}/explain`
- UI: вкладки System/Agent/**Review** (очередь подтверждения)/**Entities**,
  pin/permanent-delete, expand «why remembered», кнопка Export

## 2. Multi-personality agents (`app/agent/personalities/`)

- "Agent Profile": системный промпт, набор tools/skills, модель, настройки, свой memory namespace
- Presets: DM, Coder, Researcher, Writer, Assistant
- CRUD через UI, переключение в чате, могут вызывать друг друга как subagents

## 3. Knowledge Base / Wiki 🆕

- Организованное хранилище статей, заметок, чеклистов — отдельно от хаотичной "памяти"
- Поддержка Markdown, категорий, тегов, полнотекстового поиска
- Связь с памятью: факты из semantic memory могут "продвигаться" в KB при подтверждении пользователем
- Wiki tool для агента: `read_wiki`, `write_wiki`, `search_wiki`, `update_wiki`
- **Agent Wiki mode**: агент сам ведёт документацию по проекту/кампании/исследованию

## 4. Conversation Organization 🆕

- Теги, папки/коллекции, pinning, архивирование бесед
- Поиск по содержимому всех диалогов (не только semantic memory)
- Закреплённые сообщения внутри беседы
- Bulk-операции: архивировать, экспортировать, удалить группу бесед

## 5. Observability / Analytics (`app/observability/`)

- Лог LLM-вызовов (модель, токены, цена, latency, provider)
- Лог tool calls (name, args, result, duration, success/error)
- Дашборд: расходы, топ tools, latency, история вызовов
- OpenTelemetry-экспорт (опц.)
- Дашборд "Память": сколько фактов, типы, активность
- Использовать для этого LangSmith

> **Текущий статус (аудит):**
> - ✅ Live tail (InspectorRegistry), timeline, compare, replay
> - ✅ Tool-call лог (`ToolCall` таблица: name/args/result/duration/success)
> - ✅ LLM-call цена/токены/модель/provider (`SpendLog`) + latency (`llm_call_complete` events)
> - ✅ Дашборд памяти: факт-каунт + by_type (endpoint `/memory/stats`, теперь с `total_pending`/`total_entities`)
> - ✅ Агрегирующие дашборды: spend-over-time, spend-by-model, top-tools, latency, глобальная call-history (`/api/analytics/*`)
> - ✅ Объединённый LLM-call лог (`/api/analytics/call-history` с пагинацией и фильтрами)
> - ✅ "Активность" памяти — timeseries (`/api/analytics/memory-activity`)
> - ✅ OTel-экспорт (опц., `app/observability/otel.py`, env-gated через `OTEL_EXPORTER_ENDPOINT`)
> - ✅ Тесты на ToolCall/SpendLog/аналитику (`tests/test_analytics.py`, 21 тест)
> - ✅ Frontend: AnalyticsPage с дашбордами (`/analytics`)

## Деливерабл

Память между сессиями, разные "личности", полная аналитика, **Wiki/KB**, **организация диалогов**.

---

## Прогресс фазы 3a

| Раздел | Статус |
|--------|--------|
| 1. Long-term memory (incl. entity, confirmation, pin, export, explainability) | ✅ Завершено |
| 2. Multi-personality agents | ⏳ Ожидает |
| 3. Knowledge Base / Wiki | ⏳ Ожидает |
| 4. Conversation Organization | ⏳ Ожидает |
| 5. Observability / Analytics | ✅ Завершено |

