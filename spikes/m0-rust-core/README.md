# M0 Rust core spike

This is an isolated, disposable proof for M0 of the Rust-core migration. It is
not a production runtime and must never be pointed at `data/harness.db`.

The spike proves a narrow end-to-end path:

- JSON-RPC/JSONL App Protocol over stdio;
- a server-derived local identity; request payloads cannot override the actor;
- a server-owned capability policy; clients cannot grant themselves `write`;
- an external scripted worker that streams content and proposes a tool intent;
- bounded protocol/worker frames, message counts, event channels, and client-delivery deadlines;
- Rust-owned capability and approval decisions with an atomic approval/effect/event transaction;
- fingerprinted command idempotency and collision-safe trusted tool effects;
- append-only events in a disposable SQLite database;
- a database marker that rejects unrelated SQLite files;
- replay, cursor catch-up, worker crash containment, and restart.

Run all gates from this directory:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --all-targets
```
