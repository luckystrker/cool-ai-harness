# M6 durable state and security kernel

M6 moves the first authoritative runtime invariants into production Rust while the Python
application remains the execution backend. It does not cut over the existing Python store or agent
loop. Those remain M10 and M7 respectively.

## Ownership boundary

The new crates follow the roadmap boundary:

- `cool-state` owns SQLite transactions, run/session state, append-only events, checkpoints,
  idempotency, approvals/audit, atomic budget counters, artifact references and worker generations.
- `cool-security` owns capability policy evaluation, workspace confinement, network/SSRF policy,
  secret filtering, sanitized worker environments and the versioned at-rest secret envelope.
- `cool-core` owns worker process supervision and restart generations. The agent loop is absent.
- `cool-app-server` is still a transport. It delegates persistence and recovery to `cool-state` and
  does not contain policy or agent-loop business logic.

The default CLI database is `data/rust-core.db` (or `$COOL_DATA_DIR/rust-core.db`). Rust tables use
the `rust_` prefix. This deliberately avoids dual-writing or silently taking ownership of Python's
current SQLModel tables. A coexistence test creates a representative Python table, applies the Rust
schema, writes Rust state and proves the original row is unchanged. Because M6 creates a separate
database by default and performs no irreversible user-schema migration, the pre-cutover backup rule
does not trigger yet; M10 must add backup/restore evidence before its first write to the existing
user database.

## Durable invariants

SQLite uses foreign keys, a busy timeout and WAL for file-backed stores. Each mutation that changes
canonical state is one transaction:

1. actor ownership and idempotency fingerprint are checked;
2. the proposed state transition and sequence are validated;
3. the canonical event is appended;
4. the run projection/checkpoint and active-session pointer are updated;
5. only then is the transaction committed and the event published by the App Server.

Run transitions are typed and terminal states cannot be reopened. Event sequence is monotonic per
run and unique in SQLite. `replay_run` derives inspector state from the canonical event log and
rejects gaps or divergence from the cached run projection. Idempotency keys are scoped by actor and
operation; the same key with a changed fingerprint fails closed.

On process startup, accepted non-terminal runs cannot still have an attached executor. Recovery
therefore appends exactly one `run.failed` event with `run_interrupted` / `core_restarted`, clears the
session's active run and leaves subsequent recovery passes empty. No tool or external side effect is
replayed.

## Approvals, budgets and provenance

Approval creation, revision checking, decision, audit record and canonical approval events are
transactional. Actor binding comes from the transport boundary, never the request payload. A
compare-and-set update means one resolver wins; stale or duplicate competing revisions are rejected.
An idempotent retry of the winning request returns the stored result without another event.

Budget values use integer tokens and micro-USD rather than floating point. Limit evaluation and
counter increment happen under the same SQLite write transaction. Reaching the configured limit is
allowed; a reservation that would exceed it is rejected without changing any counter. Iteration and
proactive-action counters use the same primitive.

Artifact rows are provenance-bearing content references: actor, source, session/run, SHA-256, size
and relative storage path. M6 validates the reference and containment shape; the content-addressed
blob store and legacy artifact migration remain M10.

## Security kernel

Capabilities are the canonical `read`, `write`, `execute`, `network`, `git` and `send_external`
categories. `deny > ask > allow`; combining core, tool and plugin policy can only narrow access.

Workspace confinement rejects absolute escapes and lexical parent traversal, canonicalizes the
existing prefix, and rejects symlinks plus Windows reparse points. M7 trusted tools must consume the
returned confined path and retain platform-safe open semantics at execution time.

Network policy accepts only HTTP(S), rejects credentials/fragments, applies exact-or-subdomain
allowlists, blocks non-public IPv4/IPv6 ranges and returns the checked addresses for DNS-pinned
connection. IP-literal URLs are checked from the URL itself, so a caller cannot supply a fake public
DNS answer. Redirects must pass the same policy and a bounded redirect count; response-size and
timeout limits are carried in the policy for the M7 network client.

Secret filtering recursively covers text/JSON and removes secret-shaped worker environment names.
At-rest secrets use a JSON envelope with `version`, `keyId` and Fernet ciphertext. The reader also
accepts the raw Fernet tokens written by the Python `cryptography` implementation, derives legacy
passphrase keys with the same SHA-256 + URL-safe-base64 rule, supports an old-key reader set and
rewrites through the active key. Production rejects placeholder keys.

`fernet` is built with its pure-Rust crypto feature, so Windows/macOS/Linux builds do not depend on
a system OpenSSL installation. It is MPL-2.0; that license is explicitly allowed for this dependency
in `deny.toml`. Direct versions remain exact pins and the existing locked/cargo-deny policy applies.

## Worker lifecycle

Workers run as child processes with cleared, explicitly supplied environments and `kill_on_drop`.
The supervisor durably records `starting`, `running`, `failed` and `stopped` plus a monotonically
increasing generation. Kill/restart waits for process termination before starting the next
generation. A newly constructed core never trusts a persisted `running` row as proof that a child is
attached; recovery starts a new generation. M8 adds concrete plugin/MCP worker protocols and health
checks on top of this lifecycle.

M6's supported deployment remains one local core writer, as established by M5. Cross-process lease
and remote/server identity ownership are part of the M11 authenticated server boundary; running two
independent `cool app-server` processes against one data directory is unsupported and is not a
multi-user coordination mechanism. SQLite still serializes writes and protects file integrity.

## Verification map

- state-machine rejection before side effects: `cool-state/tests/durable.rs`;
- approval and budget races: real multi-thread contention in the same suite;
- core/App Server restart and replay: `cool-state` recovery plus
  `cool-app-server/tests/durable_restart.rs`;
- worker kill/restart and core restart: `cool-core/tests/supervisor.rs` with a real child process;
- capability/path/SSRF/secrets/Fernet parity: `cool-security/tests/parity.rs`, including a token
  emitted by Python `cryptography`;
- M5 transport regression: the complete `cool-app-server` suite remains required;
- protocol/schema/client drift: generated Rust/JSON Schema/TypeScript artifacts and golden replay.

There are no skipped critical M6 tests. HTTP fetch execution, trusted filesystem tools, provider
calls and the Python execution adapter itself remain fail-closed until their owning phases.
