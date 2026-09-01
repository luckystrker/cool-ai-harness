# M7 Rust agent and trusted tool runtime

M7 moves provider orchestration and trusted built-in tool execution into production Rust. It does
not load executable plugins or MCP servers (M8), replace the existing Python application database
(M10), or expose the Web/API server (M11).

## Ownership and event truth

- `cool-agent` owns the provider-neutral message model, `ModelDriver`, streaming loop, retries,
  cancellation, context compaction, tool registry/dispatch, planning and subagent orchestration.
- `cool-security` remains the only capability/path/network/secret policy implementation.
- `cool-state` remains the only durable writer. Conversation history is reconstructed from
  canonical `item.completed`, `tool.completed` and `tool.failed` events across session runs; there
  is no second messages table or dual-write path.
- Durable messages, tool/approval arguments and event payloads pass recursive secret masking before
  persistence or publication; current-turn execution still receives the original in-memory value.
- `cool-app-server` authenticates the actor, creates the durable run, publishes committed events,
  and bridges durable approvals back into the suspended loop.

The runtime records `run.started` before the user item and records a complete assistant item before
dispatching its tool batch. Every requested tool reaches `tool.completed` or `tool.failed`, including
batch cancellation and handler panic. A terminal run event is committed only after the last
history-bearing event.

## Provider contract

`ModelDriver` receives canonical messages and tool definitions and returns a bounded asynchronous
event stream. The core, not the provider, emits events, reserves budgets, dispatches tools and owns
retry/cancel decisions. Retry occurs only before visible progress, preventing duplicate streamed
content. OpenAI-compatible streams are consumed through the usage-only trailer and `[DONE]`, so a
preceding `finish_reason` cannot hide accounting. App Server and standalone sinks reserve token and
available cost usage atomically in the durable store. A configured cost ceiling fails closed when
the provider cannot supply cost data.

The baseline `OpenAiCompatibleDriver` uses direct HTTP/SSE over rustls. It resolves and pins the
configured provider host through `NetworkPolicy`, disables automatic redirects, caps response size
and time, reassembles fragmented tool-call arguments, and never exposes the API key to a tool or
worker environment. `cool run` requires an API key or an explicitly configured base URL unless
`--scripted` is selected for deterministic local checks. An explicit loopback URL supports local
OpenAI-compatible servers such as Ollama without an API key; that exception admits loopback only,
not other private or link-local addresses. `OPENAI_DEFAULT_MODEL` remains accepted alongside the
Rust-specific `OPENAI_MODEL` override.

## Trusted tools and fallback

The built-in registry contains confined `read_file`, `list_files`, `write_file`, argument-vector
`shell`, argument-vector `git`, and `update_plan`. Capability and per-tool decisions are combined as
`deny > ask > allow` before any process or filesystem side effect. Writes revalidate the existing
parent/target, process execution uses the workspace cwd, cleared and explicitly sanitized
environment, bounded output, timeout, kill-on-drop and secret-aware output filtering.

Filesystem confinement rejects absolute/parent escapes and existing symlink or reparse traversal,
then revalidates immediately before I/O. It is not a handle-relative filesystem capability: a
separate same-user process racing path replacement is outside the M7 single-local-core threat model
and remains a documented hardening item for the isolated worker boundary in M8.

The production App Server keeps arbitrary host processes fail-closed because M7 does not ship an
OS-isolated launcher. The handler can be exercised only through an explicit trusted-host opt-in for
parity tests or a future isolated embedding; approval alone does not enable it.

`PythonFallbackTool` is an explicit, preconfigured executable/script adapter for tools not yet
ported. The model cannot choose the interpreter or script path. Each call receives one JSON request
over stdin and the same process/environment/output bounds as native launch tools. It is not a
general plugin host and does not replace M8's worker protocol.

This phase does not claim container, namespace, Windows restricted-token, cgroup or multi-tenant
filesystem isolation. The trusted-host launcher parity fixture verifies workspace cwd,
cancellation/timeout/output bounds and host-secret stripping, while its production default remains
disabled.

## Planning, subagents and context

`update_plan` results become canonical plan events. Subagents run in a distinct durable child run
and report start/completion/failure to the parent; they never append a second terminal event to the
parent run. Context compaction preserves the system prompt, most recent exchange and complete
assistant-tool-result groups. Bounded `AGENTS.md` project instructions are injected as guidance and
cannot modify capability policy. Instruction reads use workspace confinement checks and consume at
most 16 KiB plus the truncation sentinel.

## Dependency policy

M7 adds exact pins for `async-trait`, `futures-util` and `reqwest` with rustls and no default native
TLS. The TLS graph introduces only permissive ISC, BSD-3-Clause and CDLA-Permissive-2.0 licenses;
these are added to the workspace allowlist. `cargo-deny` remains the CI enforcement point.
