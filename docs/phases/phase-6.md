# Фаза 6: Product Readiness + Backlog

> **Статус:** ⏳ Ожидает
> **Длительность:** по мере необходимости

Цель: доведение проекта до продуктового качества + реализация backlog-фич.

---

## Product Readiness

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

## Backlog Features

### Document Intelligence 🆕 (низкий приоритет)

- Продвинутый парсинг PDF/DOCX/CSV — извлечение таблиц, структуры, метаданных
- Chunking с сохранением контекста для больших документов
- `document_parse` tool: структурированное извлечение содержимого
- `document_qa` tool: ответы на вопросы по документу с цитатами
- **Document Library**: организованное хранение документов с поиском по содержимому
- Поддержка OCR для сканов (Tesseract / LLM-based)

### Data Analysis Sandbox 🆕 (низкий приоритет)

- Специальный режим для анализа CSV/Excel/JSON-данных
- Pandas-образные операции через естественный язык
- Генерация графиков и визуализаций (matplotlib/plotly)
- `data_query(df, question) → answer, chart` tool
- **Data workspace**: загрузка датасета, просмотр, преобразования, экспорт результатов
- **Auto-insight**: агент автоматически находит корреляции, аномалии, тренды

### Knowledge Base Expansion 🆕

- Расширение Wiki/KB (из Фазы 3a) с поддержкой вложений, связей между статьями, графа знаний
- **Auto-wiki**: агент автоматически ведёт Wiki на основе диалогов и research
- **Wiki export**: экспорт Wiki в статический сайт / PDF / Notion

### Export & Sharing 🆕 (без публичного шеринга)

- Экспорт диалога в PDF/Markdown/HTML
- Экспорт артефактов (research report, D&D campaign, code project)
- Bulk export всех данных пользователя (GDPR-style)
- **Share в рамках инстанса**: ссылка на конкретный run/artifact для другого пользователя того же инстанса (с токеном доступа)
- Экспорт памяти и KB в переносимом формате

### Conversation Organization (расширение) 🆕

- Умные коллекции: auto-tagging бесед по темам (через LLM)
- Advanced Search: фильтры по дате, personality, модели, tool usage, стоимости
- Conversation analytics: статистика использования, топ-темы, динамика
