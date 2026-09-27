// Single typed boundary to the Rust App Protocol facade (`cool serve`).
//
// In production the facade serves both the SPA and `/api/rpc` on the same
// origin, so the transport defaults to a relative base URL. For the split dev
// mode (`npm run dev`), Vite proxies `/api` to `cool serve` — or point
// `VITE_COOL_API_URL` at the facade explicitly.
//
// Token resolution: a `server`/container deployment requires a bearer token for
// the API surface. `VITE_COOL_API_TOKEN` is a build-time override (it bakes the
// secret into the bundle, so prefer not to ship it); otherwise the SPA accepts
// `?token=<token>` once, stores it for the tab and strips it from the URL. The
// static bundle itself is never token-gated, so the shell can load and read the
// token before the first API call.
import { CoolSdk } from "@cool-sdk/client"
import { HttpTransport, streamRunEvents, type StreamRunEventsOptions } from "@cool-sdk/http"

const TOKEN_STORAGE_KEY = "cool_api_token"

function stripTokenFromUrl(): void {
  try {
    const url = new URL(window.location.href)
    if (!url.searchParams.has("token")) return
    url.searchParams.delete("token")
    window.history.replaceState(null, "", `${url.pathname}${url.search}${url.hash}`)
  } catch {
    // A non-browser or sandboxed document has no URL to sanitize.
  }
}

function resolveToken(): string | undefined {
  const configured = (import.meta.env.VITE_COOL_API_TOKEN as string | undefined)?.trim()
  if (typeof window === "undefined") return configured
  let fromUrl: string | undefined
  try {
    fromUrl = new URL(window.location.href).searchParams.get("token")?.trim() || undefined
  } catch {
    fromUrl = undefined
  }
  let stored: string | undefined
  try {
    stored = window.sessionStorage?.getItem(TOKEN_STORAGE_KEY)?.trim() || undefined
  } catch {
    stored = undefined
  }
  if (fromUrl) {
    try {
      window.sessionStorage?.setItem(TOKEN_STORAGE_KEY, fromUrl)
    } catch {
      // Ignore: the token still applies for this page load.
    }
  }
  // Strip it whether or not it wins, so it does not linger in the URL/history.
  stripTokenFromUrl()
  return configured ?? fromUrl ?? stored
}

const configuredBaseUrl = (import.meta.env.VITE_COOL_API_URL as string | undefined)?.trim()
/** Origin of the Rust facade; empty means same-origin. */
export const coolApiBaseUrl = configuredBaseUrl ? configuredBaseUrl.replace(/\/$/, "") : ""
/** Bearer token for the API surface, also sent on the SSE stream. */
export const coolApiToken = resolveToken()
const token = coolApiToken

/** Shared transport: browsers carry one `cool_client` cookie identity. */
export const coolTransport = new HttpTransport({
  baseUrl: coolApiBaseUrl,
  ...(token ? { token } : {}),
})

/** Typed App Protocol client used by `src/api/*`. */
export const sdk = new CoolSdk(coolTransport)

/** Canonical cursor/reconnect run-event stream (`GET /api/events`). */
export { streamRunEvents }
export type { StreamRunEventsOptions }

/** Stable idempotency key for one mutating command invocation. */
export function idempotencyKey(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID()
  }
  return `cool-${Date.now()}-${Math.random().toString(36).slice(2)}`
}
