---
target: critique of the Cool AI Harness SPA
total_score: 23
max_score: 40
na_heuristics: 
p0_count: 0
p1_count: 4
timestamp: 2026-08-30T16-49-30Z
slug: frontend-src-app-tsx
---
# Design Health Score

| # | Heuristic | Score | Key Issue |
|---|---|---:|---|
| 1 | Visibility of System Status | 3 | Основные loading/error/active states ясны; Inspector теряет ориентацию в ранних состояниях. |
| 2 | Match System / Real World | 2 | Первый setup сразу раскрывает Provider, Base URL, fallback и circuit-breaker. |
| 3 | User Control and Freedom | 3 | Есть Cancel, Escape, retry и завершение override; не хватает уверенного выхода/ориентации в drawer и empty states. |
| 4 | Consistency and Standards | 3 | Система в целом цельная; Inspector early returns и отдельные custom popover patterns выбиваются. |
| 5 | Error Prevention | 2 | Budget override и numeric validation не дают достаточных guardrails. |
| 6 | Recognition Rather Than Recall | 2 | Accordion-навигация заставляет помнить, в какой группе находится один из десяти разделов. |
| 7 | Flexibility and Efficiency | 1 | Нет видимого command palette, documented shortcuts, bulk flows или быстрого переключения разделов. |
| 8 | Aesthetic and Minimalist Design | 3 | Интерфейс спокойный и сфокусированный, но setup перегружен, а визуальный язык слишком типовой. |
| 9 | Error Recognition and Recovery | 3 | Ошибки в основном понятны, retry доступен, ввод сохраняется; отдельные empty/zero states вводят в заблуждение. |
| 10 | Help and Documentation | 1 | Есть локальные подсказки, но нет контекстной помощи для технических решений. |
| **Total** |  | **23/40** | **Acceptable — фундамент рабочий, но до уверенного продукта нужны заметные UX-исправления.** |

# Design Specificity Verdict

## LLM assessment

Функционально Cool/Harness специфичен: outcomes, budgets, Inspector, permissions и durable-run vocabulary принадлежат агентному рабочему месту. Визуально он пока category-interchangeable: монохромные shadcn-карточки, Lucide icons и марка `H / Harness` подошли бы почти любому developer admin tool. Направление Operator's Notebook последовательное, спокойное и заслуживающее доверия, но главный механизм продукта — checkpoints, approvals, permissions и traceability — не формирует persistent shell. Специфичность живёт в copy и destinations, а не в композиции и interaction grammar.

## Deterministic scan

Полный `frontend/src`: 102 файла, 64 scannable UI-файла. Детектор завершился с кодом 0 и точным JSON `[]`: 0 findings, 0 rule hits, 0 locations, false positives отсутствуют. Это подтверждает отсутствие известных механических anti-patterns, но не подтверждает визуальную или UX-зрелость.

## Browser evidence

Проверены `/`, `/settings`, `/tasks`, `/inspector`, `/analytics` при 1440x900 и 390x844. Browser-only дефект, пропущенный детектором: на `/tasks` кнопка `New scheduled task` имеет bounds 258–445 px при viewport 390 px и обрезается примерно на 55 px. Mutation preflight не смог изменить `document.title` или добавить `<script>`, поэтому надёжного пользовательского overlay нет; fallback evidence — screenshots, DOM snapshots и geometry measurements.

# Overall Impression

Это уже аккуратный и в основном понятный operator UI, а не сырой набор страниц. Сильнее всего работают outcome-first старт и спокойная operational density. Главная возможность — сделать durable execution видимой структурой продукта и убрать разрыв между человеческим первым экраном и инфраструктурным setup. Сейчас интерфейс обещает контроль, но в самых чувствительных местах — подключение модели, расходные лимиты и метрики доверия — заставляет пользователя интерпретировать слишком много технических деталей или принимать слабозащищённые решения.

# What's Working

1. **Outcome-first старт.** Первый экран превращает абстрактный AI chat в понятные задачи и даёт ясную дугу Connect → Choose → Run.
2. **Спокойная operational density.** Ограниченные рабочие ширины, функциональные сигнальные цвета и сдержанная глубина хорошо реализуют Operator's Notebook без декоративных AI-клише.
3. **Хорошие основы states и responsive shell.** Retry/error copy в основном понятен; drawer имеет scrim, route awareness и Escape; settings и Inspector корректно складываются в мобильную колонку.

# Priority Issues

## 1. [P1] Первый обязательный setup раскрывает всю provider-архитектуру

**Почему это важно:** обещание «only required setup» сразу сменяется Provider, Base URL, model discovery, fallback и circuit-breaker. Первый пользователь должен сначала решить инфраструктурную задачу, а не начать полезную работу. Plaintext textarea для API key ослабляет соседнее обещание об encrypted storage.

**Исправление:** сначала спросить OpenAI / Anthropic / compatible endpoint; для preset показать только API key; использовать masked field с явным reveal; Base URL, model curation и fallback убрать в Advanced.

**Evidence:** `frontend/src/pages/SettingsPage.tsx:204-226,423-498`.

**Suggested command:** `$impeccable onboard`.

## 2. [P1] На мобильном `/tasks` обрезана основная create-action

**Почему это важно:** на 390 px CTA частично недоступна и превращается в `New schedule…`; пользователь не может уверенно прочитать главное действие страницы.

**Исправление:** разрешить header wrap/stack ниже подходящего breakpoint, растянуть CTA на мобильную ширину или сократить label только вместе с доступным полным именем.

**Evidence:** `frontend/src/pages/TasksPage.tsx:192-210`; measured right edge 445 px при viewport 390 px.

**Suggested command:** `$impeccable adapt`.

## 3. [P1] Временное отключение budget safeguard выглядит слишком обыденно

**Почему это важно:** `Number(threshold) || 80` молча превращает введённый `0` в `80`, а `Allow calls temporarily` выглядит как нейтральная outline-action без impact summary или warning confirmation. Интерфейс обещает контроль стоимости, но ослабляет его в момент максимального риска.

**Исправление:** inline range validation; видимый diff/dirty summary; warning confirmation с длительностью и затронутыми лимитами; disabled state, когда блокирующего лимита нет.

**Evidence:** `frontend/src/pages/BudgetsPage.tsx:202-266,291-314`.

**Suggested command:** `$impeccable harden`.

## 4. [P1] Analytics показывает 100% success без единого вызова

**Почему это важно:** `Tool calls 0` рядом с `Tool success 100%` — не косметическая странность, а дефект доверия к observability.

**Исправление:** при нулевом denominator показывать `—` / `No samples` и кратко объяснять, какое событие наполнит метрику.

**Evidence:** `frontend/src/pages/AnalyticsPage.tsx:136-153` и live `/analytics`.

**Suggested command:** `$impeccable clarify`.

## 5. [P2] Inspector empty/error states теряют ориентацию и следующий шаг

**Почему это важно:** loading/error returns появляются до постоянного Inspector header, а healthy empty state просит выбрать conversation, не предлагая её создать. Пользователь попадает в большую пустую область и должен сам восстановить mental model conversation → run → inspector.

**Исправление:** держать header и mode shell смонтированными во всех состояниях; добавить `Start a conversation` и объяснить, что появится после run.

**Evidence:** `frontend/src/pages/InspectorPage.tsx:80-104`.

**Suggested command:** `$impeccable onboard`.

# Persona Red Flags

**Alex — power user:** десять destinations спрятаны в accordion groups; нет command palette или документированных keyboard shortcuts. Повторяющаяся навигационная стоимость будет расти с каждой длинной сессией.

**Jordan — first-timer:** старт понятен за пять секунд, но первая обязательная action требует понять Provider, Base URL, model loading и fallback/circuit-breaker. На Inspector Jordan не получает связи между разговором, run и trace.

**Sam — accessibility-dependent:** drawer, поля и charts в основном сильны. Риски: метаданные 10–11 px, custom BudgetIndicator popover без полного набора `aria-expanded`/relationship/role и первый focus мобильного drawer попадает на группу Knowledge, а не на текущий Analytics item или close control.

# Cognitive Load

4 из 8 checklist items не пройдены — локально высокая нагрузка в setup/navigation: `one thing at a time`, `minimal choices`, `working memory`, `progressive disclosure`. Setup modal, expanded Operations navigation и Budget form превышают четыре одновременных решения. При этом root screen, chunking, grouping и основная hierarchy работают хорошо.

# Emotional Journey

Вход уверенный и outcome-oriented; encryption copy и recoverable errors создают доверие. Первая эмоциональная долина — provider setup, который резко переходит с целей пользователя на инфраструктурный жаргон. Вторая — нейтральная кнопка временного снятия расходного ограничения. Пик — выбор конкретного outcome; финальное впечатление портят противоречивая zero-sample метрика и dead-end Inspector.

# Minor Observations

- `PRODUCT.md`/`DESIGN.md` называют продукт Cool, chrome — Harness; нерешённая идентичность напрямую ограничивает design specificity.
- Mobile stepper скрывает labels через `hidden sm:inline`, оставляя зрячему пользователю только `1 · 2 · 3`.
- При недоступном backend sidebar и текущая page могут одновременно показывать отдельные большие error treatments.
- В mobile drawer нет явной close button; Escape и scrim работают, но первое фокусируемое место не объясняет текущую позицию.
- `0,00 $` при label USD локально корректно, но `USD 0.00` может быстрее читаться в operator context.

# Questions to Consider

- Что должно стать главным доказательством уникальности: persistent run-state, permissions ledger или timeline/checkpoints?
- Первый setup — это действительно «настроить provider» или «выбрать model service и вставить key»?
- Должно ли временное снятие budget block ощущаться так же серьёзно, как выдача рискованной capability?
- Может ли Inspector zero state сразу обучать главной истории продукта о traceability?
- Какое имя реально живёт в продукте: Cool или Harness, и какое обещание оно должно делать видимым?
