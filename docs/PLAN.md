# Cool AI Harness — development plan

Full roadmap from an empty repository to a complete AI-agent harness.

**Stack:** Rust trusted core (the only runtime since M12) + React SPA + SQLite;
Telegram (Bot + Web App) is planned. The legacy Python (FastAPI) server was
removed under
[ADR-0003](migration/adr/0003-remove-legacy-python-server.md); the completed
migration plan lives in [RUST_CORE_MIGRATION_PLAN.md](RUST_CORE_MIGRATION_PLAN.md).

## Status

| Phase | Status |
|-------|--------|
| Phase 0 — Foundation | ✅ Done |
| Phase 1 — MVP (agent loop + tools + chat) | ✅ Done |
| Phase 1.5 — Reliability, security, artifacts, evals, HITL | ✅ Done |
| Phase 2 — Skills + MCP + subagents + planning | ✅ Done |
| Phase 3a — Memory + personalities + observability | ✅ Done |
| Phase 3b — Recurring tasks + RSS + webhook | ✅ Done |
| Phase 4 — Workflows + multimodal + code tools | ✅ Done |
| 0.2 hardening — coding toolset, process launcher + OS sandbox, persistent
  policy rules, write diagnostics, `ask_user`, async/worktree subagents, lazy
  tools, `--mode json`, fork/rewind + FS checkpoints, multimodal vision,
  OAuth (Claude/ChatGPT/Gemini) + Gemini provider | ✅ Done (v0.2.0) |
| Phase 5 — Telegram + voice | ⏳ Planned |
| Phase 6 — Product readiness + backlog | ⏳ Planned |
| Phase 7 — UX polish + DevX | ⏳ Planned |

## Project goal

An AI-agent harness that:

- connects to LLMs via **API keys** and via **subscription services**
  (Claude Pro/Max, ChatGPT Plus, Google AI Ultra — OAuth via `cool auth`);
- ships with an **agent loop**, **tools**, **skills**, **MCP**, **subagents**;
- is driven from a **web UI** today and via **Telegram** (Bot + Web App) later;
- differentiates on **long-term memory**, **multi-personality agents**,
  **observability/analytics** and **recurring tasks (cron jobs)**;
- hosts specialized workflows: **deep research**, **coding tasks**,
  **multimodal analysis**.

## Architectural principles

- **Provider abstraction** — a single `LLMProvider` interface.
- **Multi-user readiness** — tables carry `user_id`, sessions are isolated.
- **Pluggable architecture** — tools, skills, MCP servers and subagent roles
  are registries/plugins, not hard-wired into the loop.
- **Streaming-first** — all LLM calls stream tokens (SSE/WebSocket).
- **Audit/observability** — every tool call and LLM request is logged.
- **Background-task ready** — deferred/recurring tasks from day one.
- **Security as a capability model** — separate grants for read/write/
  execute/network/git/send_external, plus persistent exec rules.
- **Durable execution** — every run has an ID, an append-only event log,
  cancellation, and budget limits.
- **Provenance and data control** — source, date, confidence, visibility scope.
- **Quality gate for agents** — evals gate prompt/tool changes.

## Phase history and remaining work

Phases 0–4 and the 0.2 hardening track are complete; the per-phase specs were
retired (they described the removed Python backend — see the
[migration checkpoints](migration/checkpoints/) for what actually shipped, and
git history for the original specs).

### Phase 5 — Telegram + voice (planned)

- Telegram Bot adapter: commands (`/chat`, `/research`, `/tasks`, `/rss`,
  `/settings`), inline buttons, streaming via message edits, long-run and
  recurring-task notifications, HITL approve/reject buttons.
- Telegram Web App: the full SPA inside Telegram, `initData` auth.
- Voice: voice-message input (Whisper/local transcription), TTS replies, a
  conversational agent personality, `transcribe_audio` tool.
- Single-user shared auth between web and Telegram.
- See [`docs/backlog/telegram-adapter.md`](backlog/telegram-adapter.md).

### Phase 6 — Product readiness + backlog (planned)

- **Product readiness:** OAuth/JWT + multi-tenant auth, backup/restore for the
  DB and artifacts, export/import of user data, a disaster-recovery runbook,
  provider resilience (capability discovery, fallbacks, rate-limit-aware
  retries, circuit breaker — partially already shipped), optional PostgreSQL
  + a separate scheduler worker for multi-instance deployments, RBAC/audit,
  secret rotation and retention policy for traces/artifacts, rate limiting,
  and a plugin marketplace with signature/permission review.
- **Backlog features (low priority):** document intelligence (structured
  PDF/DOCX/CSV parsing, `document_qa` with citations, document library, OCR);
  data-analysis sandbox (dataset workspace, natural-language queries,
  charts); knowledge-base expansion (linked wiki articles, auto-wiki,
  wiki export); export & in-instance sharing of conversations/artifacts;
  conversation organization (auto-tagging, advanced search, analytics).

### Phase 7 — UX polish + DevX (planned)

- Command palette (Ctrl/Cmd+K) with autocomplete, recent actions, fuzzy
  search, skill-registered commands and slash commands in the composer.
- Split view / canvas mode: chat beside a research report, code editor with
  file tree + diff preview, data grid, artifact preview.
- Prompt playground: A/B system-prompt testing, side-by-side model
  comparison, template variables, prompt version history, token counter,
  export, quick test.
- Conversation templates with placeholders and quick actions.
- Onboarding walkthrough, contextual help, FAQ, prompt gallery.

## Recorded deferred gaps

- **Browser automation** — isolated Playwright sessions were a Python-era
  feature; no Rust-core implementation yet.
- **Document OCR** — attachments and images reach the model, but scanned-PDF
  OCR is not implemented.
- **Deep research PDF/DOCX export** — delegated worker stub, see
  [`docs/backlog/python-workers.md`](backlog/python-workers.md).
- **Codex OAuth wire** — ChatGPT tokens authenticate the Codex Responses API
  only; the chat/completions driver reports `oauth_wire_not_supported`.
- **NetAccess::Pinned for spawned processes** — refused in v1 (needs an
  allowlist proxy; fails closed rather than granting silent full access).
