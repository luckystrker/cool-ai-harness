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
