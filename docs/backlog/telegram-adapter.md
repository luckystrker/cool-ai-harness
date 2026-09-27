# Backlog — Telegram adapter (moved out of M11)

Status: **planned / not scheduled**. This workstream was removed from
`M11 — Web cutover и compatibility workers` on 2026-09-20 and parked here. M11
now covers only the local Web cutover and the compatibility workers/executors
that do not require external credentials or infrastructure.

## Why it was moved out

The M11 plan originally folded the `server`-profile operationalization and the
Telegram adapter into the Web-cutover phase. They are separable and are blocked
on inputs the migration plan itself treats as stop-and-ask conditions:

- a real Telegram bot token and a public HTTPS endpoint (credentials +
  external infrastructure);
- a VPS/TLS/reverse-proxy boundary for the `server` profile;
- a decision on the per-install/session credential and login flow the `server`
  profile needs before a Mini App can obtain a `Secure` cookie.

None of these can be validated in the local, credential-free test environment,
so they do not belong in the M11 definition of done.

## Scope to pick up later

- Adapter on top of the `server` profile (not a separate agent runtime):
  the Rust HTTP facade is the only backend; no agent loop, approval store or
  policy engine in the adapter.
- Validate raw `Telegram.WebApp.initData` server-side: HMAC-SHA256 signature
  over the data-check-string with `HMAC_SHA256(bot_token, "WebAppData")`,
  constant-time compare, and `auth_date` freshness/expiry window. Never trust
  `initDataUnsafe`.
- Map the Telegram user id to a stable internal actor via a one-time identity
  binding; hand out a short-lived application session bound to that actor.
- Bot token stays a server-side secret; the adapter never forwards it and never
  exposes it to the browser.
- Integration tests for signature forgery, tampered fields, stale/replayed
  `auth_date`, and cross-user access (one actor cannot read another's
  session/run/approval).
- `server` profile operationalization: non-loopback bind only with mandatory
  auth + TLS (or an explicitly configured trusted reverse proxy), rate limits,
  audit actor, `--public-url`, `--trusted-proxy`/`--tls-terminated`.

## Applies to

- A new Rust adapter crate over `cool-http` — `backend/` was removed under
  ADR-0003, so this is now the only option.
- `docs/RUST_CORE_MIGRATION_PLAN.md` §5.1 `telegram` profile.
- `docs/PLAN.md` Фаза 5 (Telegram + Voice).
