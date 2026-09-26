# M12 — Rollback release

This document defines the supported way to return an installation to the
Python runtime after the M12 default cutover. It exists because the M12 exit
criteria require a *documented* rollback release: an operator must be able to
go back without improvising against the data root.

## The two rollback shapes

Which procedure applies depends on one thing only: **whether `cool store
adopt` has run on this data root.** Check with:

```bash
cool doctor --data-dir /var/lib/cool
```

(`"rustOwned": false` → not adopted; `true` → adopted.) The adoption report
written by `cool store adopt` also prints the exact backup path.

### A. Pre-adoption rollback (Python still owns `harness.db`)

Until adoption, `cool serve --legacy-store` opens the Python-owned store
**read-only** — the Rust runtime never wrote to `harness.db`. Rollback is a
pure downgrade:

1. Stop `cool serve` / the `cool` container.
2. Start the previous release's Python server the way the pre-M12 install did
   (`uvicorn app.main:app` from `backend/`, or the pre-M12 Docker image tag).
3. The Python server applies/verifies Alembic migrations on startup as usual —
   none were lost.

No backup restore is needed: the file was never modified by Rust.

### B. Post-adoption rollback (Rust owns `harness.db`)

`cool store adopt` transfers migration ownership to the Rust store. Python
(`backend/app/core/db.py`, `backend/alembic/env.py`) **refuses** to open a
Rust-owned store by design — do not bypass those guards. The supported
rollback restores the verified pre-adoption backup, which `adopt` always
writes before its first mutation:

1. Stop `cool serve` / the `cool` container.
2. Locate the backup. Default location is the data directory itself, named
   `harness.backup-<YYYYMMDDHHMMSS>.db` (the adoption report records the exact
   `backupPath`; `cool store adopt` prints it and `cool doctor` reports it).
3. Verify the backup before overwriting:

   ```bash
   sqlite3 /var/lib/cool/harness.backup-<ts>.db "PRAGMA integrity_check;"
   ```

   must print `ok`.

4. Restore it over the live file:

   ```bash
   cp /var/lib/cool/harness.backup-<ts>.db /var/lib/cool/harness.db
   ```

   (`rust-core.db` can stay — the Python runtime ignores it.)

5. Start the previous release's Python server. It sees a normal Python-owned
   `harness.db` at Alembic revision `0022` and starts normally.

Caveat: any runs/messages/memory written **after** adoption exist only in the
replaced file — they are not merged back. Keep the replaced `harness.db`
(rename rather than delete) if that data matters.

## Release policy

- Rollback to the Python runtime is supported **only on the last
  Python-containing release tag**; once ADR-0003 executes and the Python
  server is deleted, shape-B rollback to Python is no longer possible from
  that release onward — the supported path forward is a new `cool` release.
- The runtime-default cutover and the legacy-code removal are separate
  releases by plan rule (§13): this document covers only the cutover rollback.
- Adoption is never a startup side effect — `cool serve` cannot strand an
  operator on a Rust-owned store by accident.
