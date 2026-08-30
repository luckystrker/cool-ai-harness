---
version: 1
slug: "frontend-src-app-tsx"
primary_target: "frontend/src/App.tsx"
related_targets: ["frontend/src/pages/ChatPage.tsx","frontend/src/pages/SettingsPage.tsx","frontend/src/pages/BudgetsPage.tsx","frontend/src/pages/TasksPage.tsx","frontend/src/pages/AnalyticsPage.tsx","frontend/src/pages/InspectorPage.tsx"]
---

## Scope and mode

- Scope: the full Cool SPA shell and its operational routes.
- Mode: Operate.

## Audience, job, and task

- Audience: the technical owner of a local installation.
- Job: start a real AI task, supervise its authority and execution, and verify the recorded result.
- Primary task: complete one monitored run without losing the relationship between intent, permission, execution, and evidence.

## Proof and constraints

- Use only state exposed by the product; never present a conversation id as a run id or claim evidence before a run completes.
- Preserve provider abstraction, durable-run semantics, explicit permissions, budgets, and responsive access to every consequential action.
- Keep first setup progressive, critical overrides deliberate, zero-data states honest, and mobile controls inside the viewport.

## Chosen direction and memorable moment

- Direction: The Flight Ledger, selected identity for Cool; direction seed `65c17906`.
- Memorable moment: the persistent live recorder beside an active desktop conversation, adapting to a visibly labelled Record panel on smaller screens.

## Unresolved decisions

- None for this release. Dark-theme visual QA can be broadened in a later dedicated pass without changing the shipped direction.
