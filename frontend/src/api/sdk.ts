// Single typed boundary to the Rust App Protocol facade (`cool serve`).
//
// In production the facade serves both the SPA and `/api/rpc` on the same
// origin, so the transport defaults to a relative base URL. During the
// migration the Python REST/WS backend is still a fallback; point
// `VITE_COOL_API_URL` at the Rust facade (and optionally set
// `VITE_COOL_API_TOKEN`) to route commands there.
import { CoolSdk } from "@cool-sdk/client"
import { HttpTransport, streamRunEvents, type StreamRunEventsOptions } from "@cool-sdk/http"

const configuredBaseUrl = (import.meta.env.VITE_COOL_API_URL as string | undefined)?.trim()
/** Origin of the Rust facade; empty means same-origin. */
export const coolApiBaseUrl = configuredBaseUrl ? configuredBaseUrl.replace(/\/$/, "") : ""
/** Bearer token for the `server` profile, also sent on the SSE stream. */
export const coolApiToken = (import.meta.env.VITE_COOL_API_TOKEN as string | undefined)?.trim()
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
