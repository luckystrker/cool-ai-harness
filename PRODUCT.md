# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

The primary user is the owner of their own local installation: a developer or
technical power user who uses AI for coding work, deep research, material
analysis, and personal automation. The product is optimized for personal use
first, not a team SaaS scenario.

## Product Purpose

The product unifies models, tools, and the user's context into a single
workspace where a complex AI task can be started, controlled, stopped when
needed, and audited after completion. Success means the user drives a real
outcome to completion without losing track of what the agent did, which
permissions it used, and how much it spent.

## Positioning

A controllable local workspace for complex AI tasks with verifiable,
resumable runs — not just a chat with a toolset. The product's distinguishing
mechanism is durable execution: every run has state, an append-only event
log, permission control, approvals, budgets, checkpoints, and inspection /
replay tooling.

## Operating Context

- The user runs the Rust `cool` server and the React SPA in their own
  environment and connects an LLM provider via an API key, a subscription
  OAuth sign-in (`cool auth`), or a compatible endpoint.
- Work starts from a conversation or a specialized workflow; the user picks
  the project/working directory, the model, the permission mode, and
  optionally plan mode.
- The agent can work with local files and code (search/read/write/edit),
  Git/shell through the gated process launcher, documents, memory, skills,
  MCP tools, subagents (including background, worktree-isolated ones), and
  recurring tasks.
- Progress is observable via streamed events, approvals, interactive
  questions (`ask_user`), artifacts, the run journal, the inspector, and
  spend/tool analytics.
- Sessions can be forked/rewound from any message cursor, and filesystem
  checkpoints allow scoped workspace restore per run.

## Capabilities and Constraints

- The current product is a web application: a Rust trusted core (`cool`
  binary) / SQLite backend and a React 19 / TypeScript / Vite / Tailwind SPA.
- LLM access goes through the single `LLMProvider` interface only; the
  product supports OpenAI, Anthropic, Gemini, OpenAI-compatible endpoints,
  and OAuth subscriptions (Claude, ChatGPT, Gemini).
- Tools, skills, MCP servers, and subagent roles attach through registries
  and plugins rather than being hard-wired into the agent loop; rare tools
  are deferred and activated lazily (`search_tools`/`activate_tools`).
- Security is built on separate capabilities (`read`, `write`, `execute`,
  `network`, `git`, `send_external`), workspace isolation, persistent
  exec/path/domain policy rules, a process launcher (`disabled` / `host` /
  OS-sandboxed), SSRF protection, approvals, and secret masking.
- Memory has project-scoped visibility and is reachable by the agent only
  through registered memory tools.
- Roadmap phases 0–4 and the 0.2 hardening track are implemented. Telegram /
  voice, product-readiness items, and the remaining UX backlog stay planned
  and must not be described as available capabilities.
- The repository is distributed under the MIT license.
- A mandatory accessibility standard is not defined yet.

## Brand Commitments

The product name is **Cool**. That is the confirmed product identity;
`Harness` remains a technical description of the category and is not used as
a user-facing name.

## Evidence on Hand

- `README.md` — the current product description, launch instructions,
  implemented subsystems, and roadmap.
- `docs/PLAN.md` — goals, architectural principles, shipped phases, and
  future plans; `docs/migration/` — migration checkpoints and ADRs.
- `crates/` — the implementation, tests, and the deterministic eval gate
  (`cool-agent/tests/deterministic_evals.rs`) for the agent loop, security,
  and budgets.
- `frontend/src/` — the live web interface for chat, memory, research,
  profiles, analytics, tasks, budgets, subagents, settings, and inspector.
- `README.md` carries screenshots of the existing interface; the repository
  contains no confirmed user reviews, customers, public benchmark results,
  pricing, or commercial claims. Future materials must not invent them.

## Product Principles

1. **Personal usefulness before scale.** The product must first help a single
   installation owner finish real tasks without team-SaaS infrastructure.
2. **The user keeps control.** Meaningful actions are visible, bounded by
   capabilities, and may require explicit approval.
3. **The result must be verifiable.** Run state, events, cost, artifacts, and
   tool results form a single execution trace.
4. **Models and extensions are replaceable.** Providers, tools, skills, MCP,
   and subagents stay modular and create no hidden lock-in to one vendor.
5. **Long-running work must not be fragile.** Streaming, cancellation,
   checkpoints, budgets, and durable runs support complex, lengthy tasks.
