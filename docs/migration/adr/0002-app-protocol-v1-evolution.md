# ADR-0002: App Protocol v1 schema and evolution rules

- Status: Accepted for M1
- Date: 2026-08-31
- Scope: canonical commands, events, generated client types, compatibility adapters and replay

## Context

The Python runtime currently exposes several independently shaped streams: `AgentEvent` over
SSE/WebSocket, research events with a `type` tag, inspector/subagent forwarding, and approval or
breakpoint payloads. Rust, Web, TUI, ACP and Telegram must eventually observe one durable semantic
model without forcing an early breaking Web cutover.

M1 needs a source of truth that is strict enough to detect drift, but can coexist with the current
`{kind, payload}` and `{type, payload}` wire objects until Web transport cutover in M11.

## Decision

### Source of truth and generated artifacts

`crates/cool-protocol` owns App Protocol v1 Rust types. Serde names define the wire contract;
`schemars` generates `schemas/cool-protocol-v1.schema.json`, and `ts-rs` generates
`frontend/src/api/generated/cool_protocol.ts`. The generator also owns the committed golden trace
set. Generated files are never edited by hand. CI runs the generator with `--check` and fails on a
missing, extra or changed artifact.

JSON integers consumed by JavaScript are represented as TypeScript `number`. Sequence, revision
and token-count values must stay within JavaScript's safe-integer range at every browser-facing
transport boundary; future storage work must reject or encode values outside that range rather
than silently rounding them.

### Envelope and identity

Every canonical event contains `eventId`, `schemaVersion`, `sessionId`, `runId`, optional `itemId`,
per-run `seq`, `occurredAt`, actor, source, correlation/causation identifiers, a tagged typed event,
and namespaced extensions. Consumers deduplicate by stable event ID and order durable events by
per-run sequence. Cursor replay uses `runId + afterSeq`; a replayed event must reduce idempotently.

Client commands carry fixed `protocolVersion = 1`, a stable command ID and a tagged command.
Identity is absent from the client-deserializable envelope: transport authentication creates a
server-only `AuthorizedCommand`. Session ownership is derived from that actor. Every mutating
command carries a required durable idempotency key inside its parameter type; read-only commands
do not. Transport request IDs remain separate. Errors combine a JSON-RPC code, stable Cool code,
safe message/details and `retryable`.

### Compatibility projection

During incremental migration, Python adapters create a canonical envelope first and store an exact
deep-copied legacy object under the `io.github.luckystrker.cool` extension namespace. The existing
SPA object is projected from that extension. This preserves every legacy field—including fields
not yet useful to the canonical reducer—without making the legacy shape part of the trusted core.

`run_conversation_turn` binds each event to one real run-scoped adapter before
`AgentEvent.to_dict()` projects it. Research holds one adapter in its run-scoped `EventSink`, and
routes the initial `started` frame through it. The current Web, inspector and subagent consumers
therefore pass through the adapter without a wire-format change. Keepalive/end frames are typed
separately from durable events and never enter event replay.

### Version evolution

Within protocol/schema version 1:

- new optional fields and new event or command variants are additive;
- existing field names, meanings, units, nullability, tag values and stable error codes do not
  change;
- existing required fields are not removed or made optional;
- enum variants are not renamed or reused with a different meaning;
- unknown extension namespaces must be preserved by relays and ignored by consumers that do not
  understand them;
- product extensions use `io.github.luckystrker.cool`; third-party extensions use an owned reverse
  DNS namespace;
- an event becomes removable only after all supported clients stop producing and consuming it and
  its golden fixture has completed a documented deprecation window.

A change that violates these rules requires a new negotiated protocol or schema major version,
parallel generated artifacts, compatibility fixtures in both directions, and an explicit ADR.
Clients send supported protocol versions during `initialize`; the server selects one common
version or returns a stable unsupported-version error. Silent fallback is forbidden.

### Conformance gates

The twelve critical golden scenarios cover chat, parallel tools, approval/breakpoint,
cancel/reconnect with duplicate delivery, successful and failed plans, subagent,
multimodal/artifact, budget, research, worker crash/restart and error. Rust deserializes and replays every trace; TypeScript replays the
same files and must produce the committed identical `ClientState`. Python validates every event
against the generated schema and asserts complete adapter-kind coverage plus lossless projection.
Reducers sort replay input by canonical sequence, accept an exact duplicate event idempotently,
and reject gaps, mixed runs, conflicting sequence numbers, or an event ID reused with different
content. The committed expected states are authored per scenario independently of reducer code.
Approval requests carry a server-generated ID and revision before they are exposed to a client;
their explicit resolved event makes approved, denied and timed-out outcomes replayable. Plan
creation, step status and progress are also part of the shared reducer state rather than parse-only
events.

The compatibility approval registry is keyed by that server ID and scopes the model call ID by
actor, conversation and run. Resolution requires the expected revision/run ID and matching owner
scope.
The React client retains those fields from the request event; it never authorizes by model call ID.
This registry is still process-local in M1 and becomes durable with Rust run ownership in a later
phase.

Every plan execution stream starts with `plan.created`, even when the plan was created by an earlier
run. Reducers also validate the plan ID on step and progress events so a malformed mixed-plan stream
fails instead of mutating the active plan. `plan.progress` carries an explicit status; reducers must
not infer completion or failure from step counters.

## Consequences

- Rust types are authoritative even while Python still owns runtime execution.
- The current Web API remains compatible in M1; canonical transport adoption is a later phase.
- Extensions provide lossless migration evidence but are not permission to leave new durable
  semantics permanently untyped.
- Reducer changes, adapter mappings and golden expected states must change together and pass all
  three language gates.
