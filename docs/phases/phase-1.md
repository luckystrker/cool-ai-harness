# Фаза 1: MVP — Agent Loop + одна модель + чат

> **Статус:** ✅ Завершено
> **Длительность:** 1-1.5 недели

Цель: рабочий чат с агентом, который умеет вызывать инструменты.

## Backend

### 1. Provider abstraction (`app/providers/`)

- Абстрактный `LLMProvider`: `chat_completion(messages, tools, stream)`, `embed(text)`
- `OpenAIProvider` (работает с OpenAI, OpenRouter, DeepSeek, Groq, локальными через Ollama)
- `AnthropicProvider` (Claude через API)
- `ProviderRegistry` — выбирает провайдера по конфигу/модели
- **Подписочные сервисы** (`providers/subscription/`) — адаптеры для Claude Pro/Max, ChatGPT Plus (experimental)
- Унифицированная схема `messages`, `tools`, `responses`

### 2. Agent loop (`app/agent/`)

- `AgentExecutor`: LLM → tool_calls → execute → повторить → финальный ответ
- `max_iterations`, `max_tokens`, `max_cost` (защита от зацикливания)
- Streaming через async generator
- История в SQLite (Conversation → Message с role, content, tool_calls, tool_results)

### 3. Базовые tools (`app/tools/`)

- `ToolRegistry` + декоратор `@tool` (Pydantic-схема → JSON Schema)
- Встроенные: `web_search`, `web_fetch`, `python_execute` (sandboxed), `read_file`/`write_file`, `http_request`
- Логирование каждого вызова

### 4. API layer (`app/api/`)

- CRUD для conversations/messages
- `POST /api/conversations/{id}/messages` — SSE стрим
- `WebSocket /ws/chat/{conversation_id}` — real-time стрим токенов + tool events
- `GET/POST /api/settings/providers` — управление ключами (шифрование)

### 5. БД модели (`app/models/`)

- `User`, `Conversation`, `Message`, `ToolCall`, `ToolResult`, `Provider`
- Alembic миграции

## Frontend

- Чат-интерфейс со стримингом токенов
- Отображение tool calls (collapsible)
- Markdown + подсветка кода (react-markdown + rehype-highlight)
- Sidebar с беседами
- Страница настроек провайдеров

## Деливерабл

Рабочий чат с агентом, tool-calling, стриминг, история сохраняется.
