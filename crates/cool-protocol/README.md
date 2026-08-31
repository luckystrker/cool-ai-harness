# cool-protocol

Canonical, transport-neutral Cool App Protocol v1 types.

This crate contains wire commands, durable event envelopes, stable errors,
pagination/cursors, and the deterministic replay reducer. It contains no
transport, database, agent-loop, or tool-execution logic.

Generate committed artifacts from the repository root:

```bash
cargo run -p cool-protocol --bin generate
cargo run -p cool-protocol --bin generate -- --check
```
