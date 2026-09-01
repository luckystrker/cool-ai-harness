# M5 App Server and CLI skeleton

M5 establishes the production Rust process and transport boundary without moving durable state,
security policy or the agent loop ahead of their roadmap phases. The implementation is deliberately
an ephemeral echo runtime: it proves protocol and lifecycle semantics, not model execution.

## Commands

```text
cool app-server                         # App Protocol JSONL over stdio
cool app-server --transport local \
  --endpoint <socket-or-named-pipe>     # Unix socket or Windows named pipe
cool doctor                             # machine-readable M5 capability report
cool serve                              # routed, fails closed until its owning phase
cool run                                # routed, fails closed until its owning phase
```

`cool app-server` accepts generated `RpcRequest` values with method `cool.command`. The first
request on each connection must be `initialize`; the server selects protocol v1 and returns its
capabilities and transport limits. Responses and `run.event` notifications use the generated
`ServerFrame` union. The JSON Schema and both TypeScript copies are emitted by
`cool-protocol generate`; no transport crate defines a parallel wire model.

## Runtime boundary

The App Server owns only connection state, ephemeral sessions/runs, idempotency indexes, bounded
transport queues and cancellation signals. A prompt produces `run.started`, an echo
`content.delta`, and a terminal event so clients can exercise the complete stream. It does not call
an LLM, execute tools, decide permissions, persist data or resolve approvals. Those capabilities
remain absent from initialize and `approval.resolve` fails closed as `m6_not_implemented`.

Each accepted event is appended to the in-memory run before notification. `run.events` provides
cursor catch-up, and mutation retries use actor-scoped idempotency keys plus an exact input
fingerprint; changing the input while reusing a key returns `idempotency_conflict`. A session has at
most one active run and the active reference is cleared atomically with its terminal event.
Disconnect signals cancellation for runs created by that connection; reconnecting to the same
server process can load the session, retry the mutation without repeating it and replay the terminal
suffix.

Both input and output frames obey the advertised byte limit. Outbound queue delivery and underlying
I/O have deadlines; either deadline detaches the whole connection, so a client that stops reading
cannot retain handlers or runs indefinitely. Prompt event sizes are checked before a run is created,
which prevents an accepted canonical event from becoming impossible to replay.
Malformed JSON, invalid JSON-RPC envelopes and invalid command parameters are reported separately;
the generated schema and TypeScript types fix the JSON-RPC and method literal values accepted by the
runtime.

M5 accepts text prompts only. Artifact and image content parts fail closed as
`unsupported_content_part` until their trusted artifact path exists. Client cancellation reasons
are preserved in the canonical terminal event.

## Local-first security boundary

The stdio and local-socket transports derive the actor as `local-user`; clients cannot submit actor
identity. M5 does not expose TCP and does not authenticate a remote user. A local endpoint therefore
must be created in a user-controlled namespace and must not be published from a VPS. Authenticated
server profiles, durable actor mapping, HTTP/WebSocket and Telegram Web App integration belong to
later phases (M6/M11). The current boundary is suitable only for the primary local/single-user goal.

## TypeScript acceptance sample

After building `cool`, the generated-type sample launches the real stdio child process, negotiates
v1, creates a session, prompts it and asserts the ordered event sequence:

```powershell
frontend\node_modules\.bin\tsc.cmd -p sdk\typescript\tsconfig.json
node sdk\typescript\dist\sample-client.js
```

CI runs this sample and uploads a release `cool` binary on Linux, Windows and macOS. Dependency
advisory, license, source and version policy is documented in
[`M5_RUST_DEPENDENCY_POLICY.md`](M5_RUST_DEPENDENCY_POLICY.md).
