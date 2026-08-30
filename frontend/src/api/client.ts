/** Thin fetch wrapper for the harness API.

 * All paths are relative ("/api/...") and proxied to the backend by Vite in
 * dev (see vite.config.ts). In production the same paths are served by the
 * reverse proxy in front of the SPA. */

export class ApiError extends Error {
  status: number
  detail?: unknown
  constructor(status: number, message: string, detail?: unknown) {
    super(message)
    this.name = "ApiError"
    this.status = status
    this.detail = detail
  }
}

/** Turn API and network failures into copy that is safe to show in the UI.
 * Raw ApiError messages contain endpoint paths, which are useful in DevTools
 * but do not tell a user how to recover. */
export function getErrorDescription(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    const detail = error.detail
    if (typeof detail === "string" && detail.trim()) return detail.trim()
    if (detail && typeof detail === "object" && "detail" in detail) {
      const value = (detail as { detail?: unknown }).detail
      if (typeof value === "string" && value.trim()) return value.trim()
      if (Array.isArray(value)) {
        const messages = value
          .map((item) =>
            item && typeof item === "object" && "msg" in item
              ? String((item as { msg: unknown }).msg)
              : ""
          )
          .filter(Boolean)
        if (messages.length > 0) return messages.join(" ")
      }
    }
    if (error.status === 403) {
      return "This action is not permitted by the current settings."
    }
    if (error.status === 404) {
      return "This item is no longer available. Refresh the page and try again."
    }
    if (error.status === 409) {
      return "This item changed before the action completed. Refresh and try again."
    }
    if (error.status >= 500) {
      return "Cool returned an unexpected error. Try again."
    }
    return fallback
  }

  if (error instanceof TypeError && error.message.toLowerCase().includes("fetch")) {
    return "Could not reach Cool. Check that it is running locally, then try again."
  }
  if (error instanceof Error && error.message.trim()) return error.message.trim()
  return fallback
}

async function request<T>(
  path: string,
  init?: RequestInit
): Promise<T> {
  const resp = await fetch(path, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  })
  if (!resp.ok) {
    let detail: unknown
    try {
      detail = await resp.json()
    } catch {
      detail = await resp.text().catch(() => undefined)
    }
    throw new ApiError(resp.status, `API ${resp.status} on ${path}`, detail)
  }
  if (resp.status === 204) return undefined as T
  return (await resp.json()) as T
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, {
      method: "POST",
      body: body === undefined ? undefined : JSON.stringify(body),
    }),
  put: <T>(path: string, body: unknown) =>
    request<T>(path, { method: "PUT", body: JSON.stringify(body) }),
  patch: <T>(path: string, body: unknown) =>
    request<T>(path, { method: "PATCH", body: JSON.stringify(body) }),
  delete: <T>(path: string) => request<T>(path, { method: "DELETE" }),
  /** Multipart form upload (no Content-Type header — browser sets boundary). */
  upload: async <T>(path: string, formData: FormData): Promise<T> => {
    const resp = await fetch(path, { method: "POST", body: formData })
    if (!resp.ok) {
      let detail: unknown
      try {
        detail = await resp.json()
      } catch {
        detail = await resp.text().catch(() => undefined)
      }
      throw new ApiError(resp.status, `API ${resp.status} on ${path}`, detail)
    }
    return (await resp.json()) as T
  },
}
