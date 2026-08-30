# Фаза 4: Специализированные Workflows + Multimodal + Agent Constructor

> **Статус:** ✅ Завершено
> **Длительность:** 2.5-3 недели

Цель: специализированные research/code workflows, мультимодальный ввод,
браузерная автоматизация и визуальный конструктор агентов.

---

## Общие требования к специализированным workflows

- Результат — это `Artifact`, связанный с исходными файлами, run и используемой версией skill
- У research-утверждений есть кликабельные цитаты, дата получения, сохранённый фрагмент источника и пометка об уверенности/конфликте источников
- Пользователь может возобновить или повторить workflow с теми же входными данными и сравнить результаты разных моделей

## 1. Deep Research skill (`app/skills/builtin/deep-research/`)

> **Статус: ✅ Готово.** Выделенный workflow в `backend/app/research/`
> (модель `ResearchRun` + оркестратор + API + SSE-прогресс), страница
> `/deep-research`, tool `deep_research`, интеграция с recurring tasks
> (`workflow_type="deep_research"`), журнал браузерной активности и экспорт
> Markdown/HTML/PDF/DOCX.

- Декомпозиция запроса → параллельный поиск (subagents) → fetch/extract/оценка → синтез с цитированием
- Структурированный отчёт + библиография
- Сохранение в library, экспорт в PDF/DOCX
- Связь с Фазой 3b: можно запускать как recurring task (еженедельный дайджест по теме)

### Browser Automation 🆕

- Playwright/Puppeteer-based tool для "deep" research вместо простого `web_fetch`
- Скроллинг страниц, клики, заполнение форм, авторизация
- Скриншоты динамических сайтов (SPA, login-walled content)
- Извлечение structured data через селекторы/XPATH
- `browser_navigate`, `browser_click`, `browser_fill`, `browser_scroll`,
  `browser_extract`, `browser_screenshot`, `browser_close` tools
- Headless-режим в Docker, session management, stealth-режим

## 3. Code Task skill (`app/skills/builtin/code-task/`)

- Агент-кодер с sandboxed Python/Bash, persistent workspace с git
- Tools: `python_execute`, `bash_execute`, `read_file`, `write_file`, `git`, `web_search`
- Мульти-файловые проекты, автотесты

### Git Integration 🆕

- Не просто `git` tool, а полноценная работа с репозиторием вне sandbox
- Tools: `git_clone`, `git_diff`, `git_log`, `git_blame`, `git_branch`, `git_commit`, `git_push`
- **PR Review**: агент анализирует diff, пишет ревью, создаёт комментарии через GitHub API
- **GitHub Integration**: issues, PRs, Actions status, release management
- **Git-aware workspace**: sandbox автоматически клонирует репозиторий и отслеживает изменения
- **Commit message generation**: агент генерирует осмысленные commit messages на основе diff

## 4. Multimodal Input (Vision) 🆕

- Загрузка изображений и документов в web chat. Telegram использует тот же
  контракт вложений после реализации Фазы 5.
- OCR (Tesseract / LLM-based)
- Описание и анализ изображений (LLM with vision capability)
- Анализ скриншотов, диаграмм, графиков, UI-макетов
- `image_analyze` tool: передача изображения в vision-capable модель
- `ocr_extract` tool: извлечение текста из изображений/PDF
- **Multimodal в research**: агент анализирует скриншоты страниц, диаграммы из источников
- Провайдеры: OpenAI-compatible vision endpoints (включая Gemini-compatible)
  и Claude Vision

## 5. Agent Constructor 🆕

- Замена/развитие идеи "Conversation Templates"
- Визуальный конструктор агентов: выбор personality, tools, skills, модели, лимитов
- **Agent blueprints**: сохранённые конфигурации агентов под разные задачи
- **Tool composition**: создание "макро-tools" их комбинации базовых tools с валидацией
- **Share внутри инстанса**: blueprint можно пометить общим и клонировать
- **Agent playground**: быстрый запуск агента с blueprint'ом прямо из конструктора

## Frontend

- Спец-страницы для каждого workflow:
  - Deep research с прогресс-баром, цитатами, browser activity log
  - Code workflow в основном чате через sandboxed workspace и git tools
- **Multimodal**: drag-and-drop загрузка изображений, превью, результаты анализа
- **Browser automation**: лог действий браузера, скриншоты в реальном времени
- **Agent Constructor**: визуальный builder blueprint'ов и drag-and-drop
  композиция шагов macro-tools

## Деливерабл

Deep Research и Code Task workflows, мультимодальный ввод с анализом изображений
и документов, браузерная автоматизация, полноценный git/GitHub workflow и
конструктор агентов.
