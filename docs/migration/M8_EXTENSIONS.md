# M8 Rust extensions architecture

M8 moves executable extension orchestration behind the Rust trusted-core boundary without loading
plugin libraries into the core process. `cool-extensions` consumes the M3 declarative contract;
enabled M3 lock entries are read in place and missing Rust review fields mean “unreviewed”.

## Boundaries

- Plugin roots are content-hashed with link-like entries rejected. Portable `plugin.json`,
  `skills/*/SKILL.md`, root `mcp.json` and Cool namespaced hooks are parsed as data.
- A malformed skill, MCP server or hook becomes a component diagnostic. A broken enabled bundle or
  crashed MCP server is skipped independently while the App Server retains its built-in registry.
- stdio MCP starts a fresh supervised process, performs `initialize`, sends
  `notifications/initialized`, invokes one operation and reaps the child. Streamable HTTP pins DNS,
  disables implicit proxies, incrementally parses long-lived SSE, denies redirects, validates the
  negotiated protocol, sends its version header, recovers an expired session once after a
  pre-execution 404, bounds response size/time and carries the negotiated session ID. OAuth
  token acquisition stays outside the client; reserved routing/framing/session headers are denied.
- MCP tools enter `cool-agent::ToolRegistry` with `ask` as the default decision. Server annotations
  are retained as untrusted hints; absent a core-owned semantic override, a tool conservatively
  requires the full read/write/execute/network/git/send-external set. Runtime names are
  plugin/server/tool namespaced and collision checked. A core-owned `mcp-tool-policy.json` can
  enable an allow-set or disable individual runtime names; malformed policy fails closed.
- Hook execution requires an exact trust-hash review. The hash covers bundle content plus normalized
  handler, matcher, order and declared capabilities. Changed or unreviewed hooks fail closed and are
  appended to a masked JSONL audit. Core policy, declaration and plugin policy combine only by
  narrowing. Command hooks additionally require an explicit trusted-host launcher opt-in; the
  production App Server leaves raw host execution disabled until an OS-isolated launcher exists.
- Generic JSON-lines workers negotiate protocol v1 plus required capabilities, identify and
  translate Codex/Claude request envelopes, carry request IDs and absolute deadlines, accept
  cancellation, preserve structured errors, require idempotency for side effects, heartbeat and
  graceful shutdown. Environment is
  allowlisted/sanitized, messages are bounded, and process exit is a typed worker failure rather
  than a core failure.

## Compatibility and persistence

The Rust store reads the M3 `plugins.lock.json` version 1 document without adding Rust-only fields.
Existing installs, data paths, Python-compatible content hashes, enablement and
dependency/capability previews remain authoritative; exact `installations/<name>/<hash>` and
`data/<name>` bindings prevent cross-plugin data substitution. Hook reviews live in the adjacent
`hook-reviews.json`; absent values deliberately grant no trust and the Python lifecycle remains able
to read its lockfile.

Codex (`.codex-plugin/plugin.json`) and Claude (`.claude-plugin/plugin.json`, including a
manifestless single skill) layouts normalize skills and vendor `.mcp.json` into canonical data.
Request/response worker envelopes have explicit `codex.request/output` and
`claude.request/content` mappings. Vendor features without a reviewed mapping remain inactive with
per-path diagnostics; none become in-process Rust callbacks.

`plugin.status` joins the canonical App Protocol and reducer state. App Server run lifecycle invokes
the extension boundary and durably emits initial/degraded status. Existing worker lifecycle events
describe configured compatibility-worker start/failure/restart; workers are supervised with
heartbeat and restart-without-replay unknown-outcome reporting. `cool doctor` and
`cool plugin doctor <path>` expose the operational boundary and bundle diagnostics.

Terminal run state is persisted before Stop/Interrupt/SessionEnd side effects, preventing a crash
window that could replay a terminal hook after recovery. Their result stays in the masked hook
audit rather than appending a run event after the terminal boundary.

## Residual boundaries

- Local plugin installation/update remains owned by the M3 Python lifecycle CLI until the Web/API
  cutover. Rust consumes and validates its lockfile rather than dual-writing installs.
- Hook approval UI and interactive approval routing arrive with the M9 CLI/TUI surface; M8 returns a
  typed approval-required result.
- Publisher signatures, transparency logs and immutable third-party package acquisition are not
  introduced by M8. Existing content/revision pinning remains the supply-chain boundary.
- Hook audit and review state are local append/atomic files until M10 store parity moves the
  subsystem into the Rust database.
