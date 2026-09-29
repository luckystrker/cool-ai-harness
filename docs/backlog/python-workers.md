# Backlog — optional Python OCR/document/ML workers (moved out of M11)

Status: **planned / not scheduled**. This workstream was removed from
`M11 — Web cutover and compatibility workers` on 2026-09-20 and parked here. M11
keeps the Rust HTTP/App Protocol cutover, the Rust executors and the isolated
**compatibility worker protocol** (OpenCode Bun), but the optional Python
workers are deferred so the M11 definition of done stays credential-free and
does not depend on a heavy Python/ML toolchain.

## Why it was moved out

- The migration's north star (M12 exit criteria) is that a clean install runs
  the base chat/tool/MCP flow **without Python, Node or Bun**. Optional Python
  workers are by definition not required for that base flow, so they cannot
  block the default cutover.
- They pull in large, platform-specific dependencies (OCR engines, document
  parsers, embedding/ML runtimes) that are not part of the local, reproducible
  gate environment and would make packaging/CI significantly heavier.
- The core already degrades gracefully without them (text-only `ContentPart`s,
  no OCR of scanned documents, no local embeddings), so their absence does not
  regress the base product.

## Scope to pick up later

- Run OCR/document/ML work as **optional, out-of-process compatibility
  workers** over the same worker protocol/isolation as the OpenCode Bun worker:
  the core owns policy, approvals, budgets and the event log; the worker only
  transforms input to output and never gets host secrets.
- Candidate workers:
  - OCR for images and scanned PDFs (`image`/`artifact` inputs);
  - document extraction/parsing (PDF/DOCX/HTML → text/structure);
  - local embeddings for memory recall / semantic reranking;
  - optional local ML classifiers/extractors where the Rust core has no
    equivalent.
- Worker lifecycle: crash/timeout must not take down the core, must be visible
  to the user, and must respect the same secret-masking and size/time limits as
  the core tools. A worker restart must not corrupt an in-flight run.
- Permission/status surface: extend the worker review/status UI (already owed by
  M11 for the Bun worker) to the optional Python workers.
- Packaging: ship them as explicit optional extras (never on the default
  install path) with documented enablement and a clear "unsupported vendor
  semantics" signal when a worker is missing.

## Applies to

- A dedicated optional worker package — `backend/` was removed under ADR-0003,
  so the workers live out-of-process in their own package/repo and talk to the
  core over the compatibility-worker protocol.
- `crates/cool-extensions/` / the compatibility-worker protocol and the
  M11 compatibility-worker deliverables recorded in
  [`docs/migration/checkpoints/M11.md`](../migration/checkpoints/M11.md).
- The migration's completion criteria (see
  [`docs/RUST_CORE_MIGRATION_PLAN.md`](../RUST_CORE_MIGRATION_PLAN.md)):
  the base product runs without Python/Node/Bun; Python/TypeScript appear
  only as optional worker/client deps.
