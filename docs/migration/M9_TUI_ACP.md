# M9 Rust CLI/TUI and ACP cutover

M9 makes the Rust trusted core the only runtime behind the CLI surface: the interactive TUI is a
pure App Protocol client, `cool acp` is a projection over the same core, and the plugin/MCP/hook
inspection commands operate on the M3 plugin store. No Python server is involved in any M9 command.

## Command surface

| Command | Runtime path |
|---|---|
| `cool` / `cool tui` | Ratatui TUI over a spawned `cool app-server --transport stdio` child |
| `cool app-server` | M5 stdio/local-socket App Protocol server (unchanged transport contract) |
| `cool run` | non-interactive single run on the in-process agent runtime |
| `cool acp` | ACP v1 stdio adapter over a supervised `cool app-server` child (same core contract as the TUI) |
| `cool plugin install <path\|git-url> [--revision SHA]` | M3-compatible content-addressed install |
| `cool plugin list` / `validate <path>` / `doctor [name\|path]` | plugin store + loader diagnostics |
| `cool mcp list` | declared MCP servers of enabled bundles |
| `cool hooks list` | declared hooks with trust state from `hook-reviews.json` |
| `cool doctor` | runtime boundary report for M9 |
| `cool serve` | still routed with a structured fail-closed error; Web cutover is M11 |

`cool` fails closed with `tui_requires_terminal` when stdin is not a terminal, so piped or CI
invocations never block on raw mode.

## App Protocol additions

M9 adds versioned command families used by the TUI and ACP adapter. All mutating commands carry
idempotency keys and are replayed from the durable idempotency table.

```text
session.list     projectKey?, limit            -> SessionSummary[]
session.history  sessionId, limit              -> HistoryItem[] (bounded, newest kept)
session.fork     idempotencyKey, sessionId     -> forked session id
session.steer    idempotencyKey, runId, text   -> SteerAcceptedResult{runId, seq}
status.get                                     -> plugins / workers / mcpServers snapshot
```

- `session.history` reconstructs roles, tool calls/results and reasoning from the durable event log
  and trims the oldest items to fit one bounded frame; an item that cannot fit any bounded frame is
  an explicit error rather than an empty page.
- `session.fork` copies only history events (`item.completed` user/assistant, tool results) into a
  fresh terminal fork run with new event ids, sequential `seq` and `causation_id` back-references to
  the source events, preserving durable `(run, seq)` history order across multiple runs. The source
  session is never mutated and the fork is replayable by the same reducer.
- Session `title`/`projectKey` labels are bounded to 200 characters (`label_too_long`) so a single
  session cannot force `session.list` past the frame limit.
- `session.steer` appends a durable user `item.completed` event to an active run. The agent loop
  drains new user items at each iteration boundary (`EventSink::drain_steers`), so a steer reaches
  the next model request exactly once and is never double-counted with the run's initial prompt.
  A steer that is accepted while the run is finishing stays durable and is visible to the next turn
  of that session, but may no longer reach the current run's model request; clients reduce, not
  eliminate, that window with their own active-run state.
- `status.get` delegates to the configured `RunLifecycle` so extension status stays owned by the
  extension runtime rather than duplicated in transport code.

## TUI contract

- `cool-tui` is a protocol client: it holds no SQLite handle and no agent-loop logic, and it applies
  the same `cool_protocol::ClientState` reducer as the TypeScript Web client.
- A shared `cool-app-server::client::AppClient` speaks the JSON-RPC transport for TUI, ACP and
  tests. Dropping the last client handle closes the transport and wakes its reader task, so a split
  in-process transport observes EOF instead of staying half-open.
- The TUI handles streaming content/reasoning, tool lifecycle, plan progress, approvals, session
  list/resume/fork, cancel/retry/steer, model selection, slash commands, status, bracketed paste,
  resize and shutdown (an active run is cancelled on quit).
- `session.steer` is bound to plain input while a run is active; `Esc`/`Ctrl-C` cancels an active
  run and quits when idle.
- Profile/mode switching is not implemented in M9: the Rust core has no profile model yet
  (M10 owns profiles and constructor metadata parity).

## ACP cutover

`cool acp` keeps the M4 ACP v1 contract but runs entirely on the Rust core:

- `initialize` negotiates the client's advertised version and answers with pinned ACP v1,
  `loadSession: true`, disabled optional prompt/MCP capabilities and no auth methods.
- `session/new` validates an absolute `cwd` that must equal the server workspace; additional
  directories and client-supplied MCP servers fail closed.
- `session/load` replays persisted history as ACP chunks, including reasoning.
- `session/prompt` subscribes to the durable event stream before prompting, projects canonical
  events to ACP updates, maps `tool.approval_required` to `session/request_permission` and resolves
  the authoritative revisioned approval in the core; malformed or errored responses fail closed to
  deny. If the live buffer lags, the adapter recovers the dropped range from `run.events`
  pagination instead of waiting for a terminal event that may already be gone.
- `session/cancel` cancels the active core run and resolves pending permission requests, returning
  `stopReason: cancelled`.
- Responses for in-flight requests are drained before process exit so the last frame is not lost.
- Usage updates remain omitted (canonical usage is per-run while ACP defines current context usage).

The adapter keeps no session, approval or tool state: a fresh adapter over the same core replays the
same history.

## Verification

- `crates/cool-tui/tests/tui.rs`: state machine, full interactive run with approval and
  cancellation inside `TestBackend`, resize/paste/shutdown/disconnect handling, reducer conformance
  against all 12 golden traces.
- `crates/cool-acp/tests/acp.rs`: ACP handshake, durable prompt/load, approval resolution,
  fail-closed malformed permission, cancellation, boundary rejections, adapter restart and
  committed frame fixtures.
- `crates/cool-app-server/tests/sessions.rs`: protocol-visible list/history/fork/steer/status,
  steer delivery into the next model request, reconnect catch-up without gaps or repeated side
  effects.
- `backend/tests/test_acp_rust_fixtures.py`: validates the committed Rust ACP frames against the
  pinned upstream ACP v1 schema.

## Residual boundaries

- `cool serve` and embedded React assets remain M11 work; M9 does not change the Python Web runtime.
- The TUI talks to a child `cool app-server`; multi-process session ownership, remote transports and
  authentication remain M11.
- Steam/replay semantics cover a single local actor; multi-user leases and VPS profiles remain
  later phases.
