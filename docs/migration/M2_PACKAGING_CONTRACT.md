# M2 packaging contract

Status: implemented for the current Python runtime

This contract is the stable user-facing shell for the incremental Rust migration. M2 packages the
existing FastAPI backend and built React SPA together; it does not transfer runtime ownership to
Rust and it does not make the current compatibility server safe for public multi-user exposure.

## Stable launch and network surface

| Concern | Contract |
|---|---|
| Installed command | `cool serve` |
| Default local bind | `127.0.0.1:8000` |
| Container bind | `0.0.0.0:8000` inside the container; Compose publishes it only on host loopback |
| Web UI | `/` and client-side routes |
| HTTP API | `/api/*`, including `/api/health` |
| Streaming | SSE on the existing conversation message endpoint |
| WebSocket | `/ws/*` on the same origin and port |
| Process model | one application worker; process-local approvals and cancellation are not split across workers |

The release image has one entrypoint and one externally published port. A later Rust executable may
replace the implementation behind `cool serve`, but it must keep these routes and defaults or ship
an explicit compatibility/migration layer.

## Runtime filesystem layout

All mutable paths derive from `COOL_HOME`. Relative overrides are resolved below that directory.
Explicit absolute overrides remain supported.

| Variable | Source default | Release image default | Purpose |
|---|---|---|---|
| `COOL_HOME` | repository root | `/var/lib/cool` | mutable installation state root |
| `COOL_CONFIG_FILE` | `$COOL_HOME/config.yaml` | same | MCP and extension configuration |
| `DATA_DIR` | `$COOL_HOME/data` | same | database and application data |
| `DATABASE_URL` | SQLite at `$DATA_DIR/harness.db` | same | durable SQL store |
| `WORKSPACES_DIR` | `$COOL_HOME/workspaces` | same | user workspaces |
| `SKILLS_DIR` | `$COOL_HOME/skills` | same | user-installed skills |
| `ARTIFACTS_DIR` | `$DATA_DIR/artifacts` | same | content-addressed artifacts |
| `FRONTEND_DIST` | repository `frontend/dist` | `/opt/cool/frontend/dist` | immutable built SPA |

The M2 release image is validated with SQLite. A custom `DATABASE_URL` from `.env` is no longer
overwritten by Compose, but alternate engines are source/custom-image deployments that must supply
and validate their own driver; they are not an M2 release-image support claim.

The path variables in the table are configurable for source installs and direct `docker run` use.
The recommended Compose profile intentionally pins them under `/var/lib/cool`, regardless of `.env`,
so its single `cool-state` volume cannot silently lose state through an unmatched path override.

`/opt/cool` is immutable application code in the image. Docker Compose persists all of `COOL_HOME`
in the `cool-state` volume, and the application runs as the unprivileged `cool` user. Compose does
not bind the repository's state directories: this avoids silently changing host ownership on Linux.
An existing source-install workspace or user-skill tree must be copied into the volume explicitly
before cutover; it is never deleted or imported implicitly.

To import an existing local tree, stop the app, back up those directories, and run the following
after the image has been built. Each copy is explicit and read-only on the source side:

```bash
docker compose run --rm --no-deps -v ./workspaces:/import:ro cool \
  sh -c 'cp -R /import/. /var/lib/cool/workspaces/'
docker compose run --rm --no-deps -v ./skills:/import:ro cool \
  sh -c 'cp -R /import/. /var/lib/cool/skills/'
```

Inspect the copied state before removing any source directory. The migration does not delete it.
Do **not** copy an existing unversioned `data/harness.db` into the release volume: M2 does not claim
an automatic database-data migration. Production startup fails closed when it finds application
tables without an `alembic_version`, leaving the source database untouched. Keep the source install
and its backup until a version-aware export/import path is provided and verified.

Environment variables take precedence over `.env`; command-line host/port values take precedence
over `COOL_HOST`/`COOL_PORT`. `MCP_CONFIG_FILE` remains a backwards-compatible override for the MCP
configuration only, while `COOL_CONFIG_FILE` is the package-wide path.

## Deployment profiles in M2

- **Local/single-user is the supported default.** `cool serve` and Docker Compose bind host
  loopback. An empty `API_TOKEN` is therefore compatibility mode for a trusted local machine only.
- **VPS is an architectural path, not an M2 deployment option.** `API_TOKEN` protects direct API
  clients but the current SPA has no token bootstrap, so enabling it is not a server profile. The
  later `server` profile must add fail-closed startup validation, frontend authentication,
  authenticated actor identity, origin/CSRF controls, TLS/trusted-proxy policy, rate limits, and
  audit semantics before any Internet exposure.
- **Telegram Web App remains a later adapter over the server profile.** M2's same-origin HTTP/SSE/
  WebSocket layout avoids another packaging change, but Telegram `initData` validation and
  multi-user authorization are not implemented here.

## Compatibility promise for the Rust cutover

The Rust package may own startup, static-file serving, configuration, and state in later milestones.
It must preserve the command, one-origin route layout, environment names, and mutable directory
layout above. Any storage format change requires a versioned, crash-safe migration; merely replacing
the binary must not relocate or discard user data.
