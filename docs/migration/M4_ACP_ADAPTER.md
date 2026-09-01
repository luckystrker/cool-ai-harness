# M4 ACP adapter contract

## Scope

`cool acp` is a local, single-user Agent Client Protocol adapter over the current authoritative
Python runtime. It uses newline-delimited JSON-RPC 2.0 on stdin/stdout and implements ACP v1:

- `initialize` and capability negotiation;
- `session/new`, `session/load`, `session/prompt` and `session/cancel`;
- message/thought, tool, permission and plan updates;
- durable conversation, run and event persistence shared with the Web API.

ACP is a transport adapter. It calls `run_conversation_turn`; it does not execute tools, own
approval state or maintain a second conversation store.

The wire oracle is the upstream ACP v1 schema at revision
`4effcc11e117c67feb5ed505b17f75537932f5a6`, vendored under `schemas/` with provenance and a
content hash. This is intentionally v1 even though upstream also develops ACP v2: the migration
roadmap pins M4 to v1, and the later Rust cutover must make any version expansion explicit.

## Zed configuration

After installing the package so `cool` is on `PATH`, add a custom external agent to Zed:

```json
{
  "agent_servers": {
    "Cool": {
      "type": "custom",
      "command": "cool",
      "args": ["acp"],
      "env": {}
    }
  }
}
```

Start a new External Agent thread named `Cool`. `stdout` is reserved for ACP frames; structured
runtime logs are routed to `stderr`.

## Supported boundary

- Session IDs are stable `conversation:<id>` references to the same rows used by the Web UI.
- `session/load` replays persisted user/assistant/thought/tool history and then allows another
  prompt against that conversation.
- Each prompt creates one durable `AgentRun`; the authoritative runner appends its events to
  `run_events`, and the same bound event stream is projected through the canonical envelope to ACP.
- ACP permission responses resolve the existing scoped, revisioned approval ticket. Malformed or
  errored client responses fail closed. A cancel interrupts provider/tool waits, denies only the
  active run's pending approvals and returns ACP `stopReason: cancelled`.
- Baseline `text` and `resource_link` prompt blocks are accepted. Resource links are preserved as
  references in model context; the adapter never fetches them implicitly around file/network
  capability checks. Optional image/audio/embedded-resource capabilities remain disabled.
- A process-wide lease prevents overlapping ACP, SSE or WebSocket turns for the same conversation
  in the local single-process runtime. Shared multi-process leases remain a VPS deployment gate.
- One working directory is supported. Additional directories and client-supplied MCP servers fail
  explicitly instead of being silently ignored or expanding process authority. Native Cool MCP
  configuration remains available to the underlying runtime.
- The stdio server bounds each input frame at 1 MiB and supports concurrent responses,
  notifications and JSON-RPC batches. It rejects non-finite JSON numbers and excessive nesting.
- ACP `usage_update` is deliberately omitted: the canonical event currently reports per-call/run
  totals, while ACP v1 defines current session-context usage. Publishing an invented sum would be
  misleading; M5's context authority must add it when the value is authoritative.

This command is not a network listener and does not add VPS authentication, multi-user isolation or
Telegram identity. Those remain later roadmap gates.

## Compatibility evidence

`backend/tests/test_acp.py` validates every captured and generated frame against the vendored
official schema. It runs Zed-shaped and second reference-client handshakes, an actual
`python -m app.cli acp` subprocess initialization smoke, durable new/prompt/load, approval,
cancellation (including a hung provider), history replay, baseline resource links, concurrent-turn
rejection, strict JSON parsing and batch handling.

The automated fixtures are protocol-shape evidence, not a claim that a real Zed prompt succeeded.
For a manual Zed run, use `dev: open acp logs` in Zed and retain the initialization, session/new
and first prompt frames with the checkpoint evidence. The real-client smoke result belongs in the
M4 checkpoint and must remain explicit if the local GUI is unavailable.
