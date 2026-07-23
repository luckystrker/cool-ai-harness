import { api } from "./client"
import type { Artifact, ArtifactDetail, ArtifactUploadResponse } from "./types"

export const artifactsApi = {
  /** Upload a file as an artifact attached to a conversation. */
  upload: (convId: number, file: File, opts?: { run_id?: number; kind?: string }) => {
    const fd = new FormData()
    fd.append("file", file)
    const qs = new URLSearchParams()
    if (opts?.run_id != null) qs.set("run_id", String(opts.run_id))
    if (opts?.kind) qs.set("kind", opts.kind)
    const query = qs.toString()
    return api.upload<ArtifactUploadResponse>(
      `/api/conversations/${convId}/artifacts${query ? `?${query}` : ""}`,
      fd
    )
  },

  /** List artifacts for a conversation (newest first). */
  list: (convId: number, params?: { run_id?: number; kind?: string; limit?: number }) => {
    const qs = new URLSearchParams()
    if (params?.run_id != null) qs.set("run_id", String(params.run_id))
    if (params?.kind) qs.set("kind", params.kind)
    if (params?.limit != null) qs.set("limit", String(params.limit))
    const query = qs.toString()
    return api.get<Artifact[]>(
      `/api/conversations/${convId}/artifacts${query ? `?${query}` : ""}`
    )
  },

  /** Get artifact detail (includes extracted_text and version chain). */
  get: (convId: number, artifactId: number) =>
    api.get<ArtifactDetail>(`/api/conversations/${convId}/artifacts/${artifactId}`),

  /** Download URL for an artifact's raw file content. */
  downloadUrl: (convId: number, artifactId: number) =>
    `/api/conversations/${convId}/artifacts/${artifactId}/download`,

  /** Soft-delete an artifact. */
  delete: (convId: number, artifactId: number) =>
    api.delete<{ deleted: number }>(`/api/conversations/${convId}/artifacts/${artifactId}`),
}
