# Фаза 2: Skills + MCP + Subagents + Planning Mode

> **Статус:** ⏳ Ожидает
> **Длительность:** 1.5-2 недели

Цель: подключаемые навыки, внешние MCP-инструменты, дочерние агенты и планирование.

---

## 1. Planning Mode 🆕

Перед выполнением сложной задачи агент строит пошаговый план и показывает его пользователю.

- **Plan generation**: LLM декомпозирует запрос на шаги с зависимостями (DAG), для каждого шага указывает нужные tools/skills/субагентов
- **Plan review UI**: пользователь видит план, может редактировать шаги, менять порядок, удалять/добавлять
- **Plan approval workflow**: утверждение плана → выполнение step by step с прогресс-баром; возможность перепланирования на лету
- **Plan persistence**: план сохраняется как часть `AgentRun`, можно вернуться к нему позже
- **Plan templates**: сохранённые шаблоны планов для типовых задач (research, code review, документирование)
- **Subplan delegation**: план может содержать подпланы для subagents

## 2. Жизненный цикл плагинов и MCP

- Manifest с версией, совместимостью, автором, требуемыми capabilities и схемой конфигурации
- Установка с pinning версий, health-check, явное включение/отключение и безопасный rollback
- MCP-сервер получает только выданные ему secrets и scopes; tool description и разрешения отображаются в UI
- Subagent наследует минимальный набор capabilities, отдельный бюджет и cancellation token

## 3. Skills система (`app/skills/`)

- Концепция как у ZCode: `Skill` = директория с `SKILL.md` + опц. скрипты/ресурсы
- `SkillRegistry` загружает из builtin + user + плагинов
- Автоматическое определение релевантного skill (описание + embedding similarity)
- Встроенные: `deep-research`, `code-task`, `summarize-document`, `translate`, `brainstorm`

## 4. MCP клиент (`app/mcp/`)

- MCP-клиент (stdio + HTTP), инструменты автоматически регистрируются в `ToolRegistry`
- Конфиг через `config.yaml` или UI

## 5. Subagents (`app/agent/subagents/`)

- Агент порождает подзадачу → новый `AgentExecutor` со своим контекстом/tools/skills/моделью
- Параллельное выполнение, `Task` tool: `spawn_subagent(prompt, tools, model) → result`
- **Subagent plan integration**: subagent получает подплан от родительского Planning Mode

## Frontend

- UI для skills и MCP-серверов
- Отображение subagent-активности в чате
- **Planning Mode UI**: просмотр/редактирование/утверждение плана, прогресс-бар выполнения
- **Plan Templates**: управление шаблонами планов

## Деливерабл

Авто-активация skills, внешние MCP-инструменты, subagents для сложных задач, **планирование с утверждением пользователем**.
