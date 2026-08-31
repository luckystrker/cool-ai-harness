# M0 ownership map

This map records target ownership; it does not authorize deleting or moving the current Python
implementation. A row moves only when its phase checkpoint demonstrates parity.

| Current owner | Target owner | Compatibility/retention | Earliest phase |
|---|---|---|---|
| `backend/app/agent/*` run loop, runners, registry, planning, subagents, context | `cool-core`, `cool-protocol` | Python loop remains runtime behind adapter until Rust eval parity | M6-M7 |
| `backend/app/agent/events.py` and transport-specific event shapes | `cool-protocol` | Python adapters emit canonical types; frontend consumes generated SDK | M1 |
| `backend/app/providers/*` | `cool-providers` plus supervised provider workers | OpenAI-compatible baseline in Rust; fast-moving/experimental adapters may remain workers | M7-M8 |
| `backend/app/security/*`, approvals and budgets | `cool-security`, `cool-sandbox` | Python decisions remain authoritative until atomic/security parity | M6 |
| file, shell, code and git tools | `cool-tools`, `cool-sandbox` | Python fallback only for tools not yet ported; never dual execute | M7 |
| browser automation and optional document/OCR/data tooling | trusted browser adapter plus isolated Python worker | Playwright/document ecosystems may remain optional workers | M8-M11 |
| tool registry, skills and MCP | `cool-tools`, `cool-plugins`, `cool-mcp` | Agent Plugins 1.0 + Cool namespace; Python loader retained through parity | M3, M8 |
| hooks and executable compatibility | `cool-hooks`, supervised workers | Codex/Claude declarative adapters; OpenCode only in Bun worker | M8 |
| SQLModel models and Alembic | `cool-state` | Alembic sole migration owner until M10; Rust uses DB copies read-only before gate | M10 |
| run events, approvals, artifacts and inspector | `cool-state`, `cool-artifacts`, `cool-observability` | Canonical event adapter and replay bridge preserve existing UI | M1, M6, M10 |
| memory records, FTS/vector metadata and lifecycle | `cool-state`; optional embedding/index worker | Memory remains project-scoped and accessible through tools | M10 |
| tasks, scheduler, RSS, webhooks and wiki | `cool-core`, `cool-state`, protocol domain handlers | Python scheduler remains single owner until restart/misfire parity | M10 |
| research orchestration | `cool-core` plus provider/tool contracts | Existing dedicated SSE receives canonical adapter; optional Python research helpers allowed | M1, M7, M10 |
| FastAPI REST/SSE/WebSocket | `cool-http` facade over App Protocol | No transport handler owns business logic | M5, M11 |
| React SPA and view state | `frontend`, generated TypeScript SDK/reducer | React remains TypeScript and never accesses SQLite | M1, M11 |
| future CLI/TUI/ACP | `cool-cli`, `cool-tui`, `cool-acp` | All are protocol clients/adapters over one runtime | M4, M5, M9 |
| Telegram Bot/Mini App | server-profile adapter over `cool-http`/App Protocol | Local default remains single-user; Telegram identity maps to core actor | after M5, complete by M11 |
| OCR, PDF/DOCX, data science and user Python execution | optional isolated Python workers | Not required for base install; explicit capabilities and sanitized environment | M8-M12 |

## Ownership rules

- A client, adapter or worker may propose an intent but cannot grant itself capabilities.
- Only the active runtime writes canonical run/state tables. Replay is read-only.
- Workers never receive a DB handle, master secret key or ambient environment.
- Frontend/API compatibility is measured from generated protocol contracts and golden replay, not
  duplicated hand-written DTOs.
- Removing Python ownership requires the phase checkpoint named in the last column, not merely a
  compiling Rust implementation.
