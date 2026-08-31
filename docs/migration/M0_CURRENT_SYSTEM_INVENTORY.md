# M0 current-system inventory

Snapshot date: 2026-08-31. This inventory is the baseline for M1 golden contracts and later parity
work. Counts are evidence helpers, not stable API promises.

## Measured surface

- 166 FastAPI route decorator declarations under `backend/app/api`, including two WebSocket routes.
- 23 `AgentEvent` kinds in `backend/app/agent/events.py`.
- A separate Deep Research SSE vocabulary not represented by `AgentEvent`.
- 57 statically registered built-in tool names under `backend/app/tools` (dynamic MCP tools are
  additional).
- 26 SQLModel tables under `backend/app/models` plus FTS5/`sqlite-vec` objects managed outside
  `create_all`.
- Alembic is linear and currently reports one head: `0022`. Revision `4869d4c0322a` is the parent
  inserted between `0020` and `0021` for research runs.

M1 must generate this inventory mechanically and fail CI when a new client-visible event or API
surface appears without an explicit canonical mapping.

## Client/API families

| Family | Current transport/state | M1/M10 protocol responsibility |
|---|---|---|
| health and system prompt | REST | server health/config query and versioned config mutation |
| conversations/messages/search/bulk | REST + POST SSE + chat WebSocket | sessions/items, prompt, compact, search and organization commands |
| runs/events/cancel | REST + inspector WebSocket | run queries, cancel, cursor replay/subscription |
| approvals and breakpoints | REST response + agent events | revisioned approval/elicitation with actor/source |
| plans/templates | REST + SSE execution events | durable plan and step commands/events |
| subagent roles/runs/batch | REST + dedicated SSE | role definitions, child-run relationships and canonical progress |
| providers/models/settings | REST + encrypted DB rows | provider config, capability discovery and secret references |
| budgets/spend/analytics | REST + budget events | atomic limits, usage events and read projections |
| artifacts/uploads/downloads | REST/blob responses | metadata via protocol; bytes through authenticated bounded blob transport |
| skills and MCP store/runtime | REST + dynamic tools | plugin/skill/MCP lifecycle, diagnostics and status events |
| memory/entities | REST + FTS/vector stores | project-scoped records, review/explain and tool-only model access |
| tasks/scheduler/inbox | REST + background runs | schedule/misfire/overlap/catch-up commands and run events |
| RSS | REST + background fetch | subscriptions/entries and bounded network policy |
| webhooks | authenticated REST + public HMAC ingress | endpoint/event state, replay idempotency and external actor/source |
| wiki | REST | article CRUD/search/promote and project/user ownership |
| workspace/git UI | REST | confined directory/project identity and trusted git operations |
| Deep Research | REST + separate POST SSE | canonical research run/stage/source/subquestion/artifact events |
| Agent Constructor/macros | REST + dynamic tool registry | macro metadata, validated tool graph and capability union |

Static files and artifact byte downloads may remain HTTP-specific transports, but authorization,
metadata and lifecycle still come from core-owned commands/queries.

## Current stream vocabularies

### AgentEvent

`start`, `thinking`, `token`, `tool_call_start`, `tool_call_delta`,
`tool_approval_request`, `tool_result`, `message`, `finish`, `error`, `budget_alert`,
`react_thought`, `react_action`, `react_observation`, `llm_call_complete`, `plan_generated`,
`plan_step_start`, `plan_step_complete`, `plan_progress`, `subagent_started`,
`subagent_progress`, `subagent_completed`, `subagent_failed`.

The payload is currently a free-form dictionary and the SSE parser accepts both the nested
`{kind,payload}` shape and flat payloads. M1 must preserve semantic fields while replacing this
permissiveness with tagged generated types and explicit compatibility adapters.

### Deep Research

Observed emissions include `stage`, `source_found`, `subquestion_started`,
`subquestion_completed`, `completed`, `failed` and `cancelled`. This surface bypasses `AgentEvent`
today and therefore requires its own M1 adapter/golden traces rather than being assumed covered by
chat tests.

### Known ordering/durability debt

`append_run_events` calculates `MAX(seq) + 1` in application code and the SQLModel table does not
declare a unique `(run_id, seq)` constraint. It is adequate for the current mostly single-writer
path but is not a concurrency contract. M6 must use an atomic database-owned sequence/allocation
strategy and repair/validation for imported rows.

Current SSE is request-scoped and has no durable cursor handshake. The canonical protocol adds
`after_seq` catch-up before live tail; reconnect may not re-run a prompt or tool.

## Provider parity inventory

| Provider path | Current behavior to preserve | Rust disposition |
|---|---|---|
| OpenAI-compatible (`openai`, OpenRouter, DeepSeek, Groq, Ollama/custom base URL) | streaming/non-streaming chat, text and reasoning deltas, streamed tool-call fragments, usage/finish reason, model listing, embeddings | In-process Rust baseline; endpoint-specific deviations covered by recorded fixtures |
| Anthropic Messages API | native auth/version headers, message/tool translation, streaming content/tool events, usage/finish reason, model listing | Required Rust parity after OpenAI-compatible baseline |
| `subscription/chatgpt_plus` | experimental internal subscription endpoint and streaming adapter | Optional compatibility worker; not required for base install |
| `subscription/claude_pro` | experimental subscription endpoint; tool support is explicitly uncertain | Optional compatibility worker with unsupported-feature diagnostics |
| `ScriptedProvider` | deterministic chat/tool/cost/safety/research evals | Native Rust test driver and blocking parity gate |
| `ResilientProvider` | retry with jitter for timeout/network/429/5xx, circuit breaker and ordered fallback; never retry after first streamed event | Core-owned retry/fallback state machine; no duplicate deltas/side effects |

The Rust `ModelDriver` contract must cover canonical multimodal text/image content, tool specs and
calls, provider-visible reasoning, usage/cost, model discovery, cancellation and structured errors.
Embedding remains an optional capability and may use a worker, but base chat cannot require
Python/Node/Bun.

## Data inventory

Canonical relational records currently include users, conversations/messages/tool calls,
agent runs/events, approvals, artifacts, budgets/spend, providers, plans/steps/templates, subagent
roles/runs, memory/episodes/working memory/entities/relations/embeddings, profiles, wiki, scheduled
tasks/runs, RSS subscriptions/entries, webhooks/events, research runs and Agent Constructor macros.

Additional state outside ordinary SQLModel tables includes:

- FTS5 mirrors and optional `sqlite-vec` virtual table;
- content-addressed artifact bytes;
- working-memory scratch/state and project identity derived from working directory;
- MCP/skills/plugin configuration and runtime registries;
- scheduler and in-process run/circuit-breaker registries;
- encrypted provider credentials using Fernet.

M10 data fixtures must include every Alembic revision supported for upgrade, production-like rows,
missing `sqlite-vec`, corrupted/incomplete runs, artifacts, encrypted secrets and interrupted
migration recovery.

## Security baseline

Capabilities are `read`, `write`, `execute`, `network`, `git` and `send_external`. Effective tool
permission is the stricter of capability and per-tool decisions (`allow < ask < deny`). File tools
confine paths to configured workspaces; network tools apply SSRF policy; subprocesses use sandbox
and environment filtering; secrets are masked; approval audits and spend limits are persisted.

Current HTTP auth is optional single-user bearer `API_TOKEN`; an empty token disables checks in
development. WebSocket auth uses a query parameter. This is a compatibility baseline only. Rust
`local` adds loopback/origin/session protection, while non-loopback `server` fails closed and
Telegram identity is derived only from validated server-side Mini App data.

## Plugin/extension baseline

- Portable Tier 1: Agent Plugins 1.0.0 fixed `plugin.json`, `skills/`, `mcp.json` plus Agent Skills
  and MCP conformance.
- Cool extension namespace: `io.github.luckystrker.cool`.
- Tier 2 adapters: Codex declarative plugins and a documented Claude declarative subset.
- Tier 3: vendor metadata/commands/LSP with explicit transformed/ignored diagnostics.
- Tier 4: OpenCode executable JavaScript/TypeScript only in an isolated Bun worker.

M0 does not implement plugin loading. M3 pins upstream schemas and fixtures; M8 implements the Rust
loader/supervisor.
