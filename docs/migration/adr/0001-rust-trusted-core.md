# ADR-0001: Rust trusted core and incremental replacement

- Status: Accepted for M0
- Date: 2026-08-31
- Scope: architecture and risk decisions required before canonical protocol work

## Context

Cool is currently a Python 3.12/FastAPI application with a React/TypeScript client, SQLite state,
durable agent runs, capability checks, approvals, background jobs, memory, plugins/skills/MCP and
multiple client-visible streaming surfaces. Replacing this runtime in one step would combine
protocol, state, security, packaging and product migration risks.

The target therefore keeps the Python runtime releasable while a Rust process incrementally takes
ownership of security-sensitive and durable execution. The migration must preserve the invariants
and parity gates in `docs/RUST_CORE_MIGRATION_PLAN.md`.

## Decision

### Core boundary

Rust becomes the trusted owner of run/session state, append-only events, approvals, budgets,
capability decisions, worker supervision, dangerous built-in tools and (after M10) SQLite
migrations. React, ACP and Telegram are clients/adapters. Executable TypeScript/Python/plugin code
runs out of process and cannot access the core database, secret store or policy state directly.

No side-effecting shadow execution and no SQLite dual-write are permitted. Python remains the
default runtime until the corresponding Rust phase passes functional, data and security parity.

### Toolchain and dependency policy

- Initial pinned toolchain and MSRV: Rust 1.98.0, edition 2024, recorded in
  `rust-toolchain.toml`. Lowering MSRV later requires an explicit supported-version CI job.
- Tokio owns async I/O, cancellation and process supervision.
- Serde defines wire serialization.
- `rusqlite` with bundled SQLite is the selected state driver. Production state access will use a
  dedicated database owner/command queue and explicit blocking boundary rather than sharing a
  `Connection` across async tasks.
- `schemars` will generate JSON Schema from the same Serde Rust types in M1. `ts-rs` will generate
  TypeScript declarations from those types. Serialization fixtures and a generated-artifact diff
  gate are mandatory because neither generator alone proves semantic equivalence.
- Unsafe code is forbidden in first-party crates. Dependency licenses, advisories, duplicate
  versions and sources become CI policy no later than M5.

M0 uses exact dependency versions in its isolated Cargo lockfile. Production dependency versions
are selected and locked again in M1 rather than inheriting the spike accidentally.

### App Protocol

- JSON-RPC 2.0 is the message model.
- stdio uses one complete JSON object per line. Local sockets use the same messages with explicit
  frame boundaries. Browser HTTP/SSE/WebSocket remains a projection of the same commands/events.
- `initialize` negotiates protocol version, client instance, capabilities and limits.
- Transport request IDs correlate one connection only. Durable command idempotency keys are
  separate and scoped by authenticated actor.
- Mutating requests return their original result when actor + idempotency key is repeated.
- Errors contain the JSON-RPC numeric code plus stable Cool code, safe message/details and
  `retryable`; internal errors and secrets never cross the boundary.
- Every durable event has stable id, schema version, session/run identity, monotonic per-run
  sequence, timestamp, actor/source and correlation/causation metadata.
- Client-visible deltas remain durable through compatibility cutover because the Python event log
  currently persists them. Any later compaction/archive policy requires an ADR and must preserve
  deterministic client replay.
- Persist precedes publish. Reconnect reads `after_seq` from SQLite and then tails live events; a
  transport frame is never the authority for ordering or exactly-once side effects.
- Internal domain commands and storage rows are not exposed directly as protocol DTOs.

The M0 spike intentionally implements only `initialize`, `session.prompt`, `approval.resolve`,
`run.get`, `run.events` and snapshot `run.catchup`. Full live-subscription method families and the
gap-free catch-up-to-tail handoff belong to M1/M5/M10.

### State and migrations

- Python/Alembic remains the sole migration owner through M9. Verified current head is `0022`.
- Before M10, Rust reads only copied fixtures of the application DB. M0 writes solely to a
  disposable spike schema.
- Rust write ownership begins only after backup/restore, interrupted migration and schema revision
  compatibility tests pass.
- Production SQLite behavior must set and test foreign keys, journal/synchronous mode, busy
  timeout, transaction boundaries and uniqueness of `(run_id, seq)`.
- The existing Python `run_events` table has no database unique constraint on `(run_id, seq)` and
  computes the next sequence with `MAX(seq)`. Rust must not copy that concurrency behavior; M6 owns
  the atomic append implementation and compatibility repair strategy.

### Secrets

Current provider secrets use Fernet and may derive a key with SHA-256 from `SECRET_KEY`. Rust may
not rewrite or decrypt those rows in place during early phases. M6 adds versioned ciphertext,
legacy decrypt fixtures, rotation and failure behavior. Plaintext must never enter events, traces,
logs, generated fixtures or worker environment except for the minimum credential explicitly
granted to a provider process.

### Deployment and identity

- `local` is the default and first product target: one OS user, loopback-only Web, OS-protected
  stdio/local socket and per-install/session browser credentials.
- `server` is explicit opt-in for VPS and fails closed without authentication plus TLS or a trusted
  reverse-proxy boundary.
- `telegram` is an adapter over `server`. It validates raw Telegram Mini App `initData`, freshness
  and replay, maps Telegram user identity to a stable Cool actor and mints a short-lived Cool
  session. It never owns a second agent loop or policy store.
- Actor/owner fields exist from M1 even though the initial UI is single-user. Payload identity is
  never trusted over transport-authenticated identity.

### Plugin compatibility

Portable conformance targets Agent Plugins 1.0.0, Agent Skills and pinned MCP schemas. The Cool
client extension namespace is permanently `io.github.luckystrker.cool`; hooks and Cool-only assets
live under that namespace and are not claimed as portable Tier 1 components. Codex, Claude and
OpenCode formats remain explicit compatibility adapters with diagnostics.

### Runtime selection

The eventual runtime selector has exactly these modes before default cutover:

- `python`: current production behavior;
- `rust`: only enabled after the relevant phase checkpoint;
- `replay`: read-only fixture/client comparison, with no external side effects.

There is no side-effecting `shadow` mode. Unknown or unavailable modes fail closed rather than
silently selecting another runtime.

## Vertical spike result

`spikes/m0-rust-core` proves the highest-risk slice with a real JSONL subprocess boundary:

1. App Server creates an idempotent run in disposable SQLite.
2. A supervised external worker streams content and proposes a typed tool intent.
3. Rust derives the local actor and `write` policy from server configuration; the request cannot
   grant itself a capability.
4. A revisioned approval atomically records the decision, trusted effect, terminal state and
   durable events. Failpoints prove rollback at every write boundary.
5. Every event is persisted before being emitted to the client.
6. Replay produces the same reducer state as live events.
7. Cursor catch-up returns every event after `after_seq` without re-running the tool.
8. Worker exit 42 becomes durable `worker.failed`/`run.failed`; a new core process replays the same
   terminal state and can supervise a later run.
9. Command fingerprints reject reuse of an idempotency key with different input and return the
   original durable command result on an identical retry.
10. Broken or non-reading protocol clients are detached after a bounded delivery deadline while
    durable execution continues; an incomplete prompt without a receipt is closed deterministically
    on same-key recovery without starting a second worker.

## Spike code that must be replaced

The spike is evidence, not a production shortcut. M1/M5/M6 must replace:

- its ad-hoc partial protocol structs with generated `cool-protocol` types;
- one-connection-per-operation storage with the production database owner;
- the single `write_marker` policy with the canonical capability/approval engine;
- the in-database marker effect with a durable intent/outbox/reconciliation design before any
  external side effect is considered crash-consistent;
- fixed five-second worker deadline with negotiated deadlines/cancellation/heartbeat;
- the single scripted worker with a versioned worker handshake and environment allowlist;
- the bounded single-client event channel and snapshot catch-up with multi-subscriber queues and a
  gap-free catch-up-to-live handoff;
- its disposable schema with read-only compatibility fixtures, then versioned Rust migrations at
  the M10 ownership cutover.

## Consequences

The first Rust release is intentionally local-first, but durable identity and protocol design do
not block later VPS/Telegram deployment. More infrastructure is required up front for protocol
generation, replay fixtures and isolation, but security decisions and state transitions remain in
one auditable process and clients do not acquire runtime business logic.
