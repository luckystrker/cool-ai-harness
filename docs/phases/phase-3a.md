# Фаза 3a: Memory + Personalities + Observability + Knowledge Management

> **Статус:** ⏳ Ожидает
> **Длительность:** 2 недели

Цель: долговременная память между сессиями, разные "личности" агента, полная аналитика и система знаний.

---

## Сквозные требования к памяти и аналитике

- Каждая запись памяти имеет source/provenance, дату, confidence, namespace, TTL и статус подтверждения пользователем
- В UI: «почему это запомнено?», редактирование, закрепление, forget, экспорт и удаление всех данных пользователя
- Извлечение памяти дедуплицируется и не перезаписывает подтверждённые пользователем факты без явного согласия
- Трасса run связывает LLM-вызовы, tool calls, subagents, approvals и артефакты в единое дерево; её можно открыть и воспроизвести

## 1. Long-term memory (`app/memory/`)

- **Working memory** — контекст беседы (auto-summarization при превышении лимита)
- **Episodic memory** — все значимые взаимодействия с embeddings, semantic search
- **Semantic memory** — факты о пользователе (авто-извлечение)
- **Entity memory** — именованные сущности с атрибутами и связями
- Хранилище: SQLite + sqlite-vec (позже Qdrant)
- Memory tool для явного поиска/обновления
- UI: просмотр и редактирование памяти

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

## Деливерабл

Память между сессиями, разные "личности", полная аналитика, **Wiki/KB**, **организация диалогов**.
