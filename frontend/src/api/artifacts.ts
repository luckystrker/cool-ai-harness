import { api } from "./client"
import { idempotencyKey, sdk } from "./sdk"
import { toArtifact, toArtifactDetail } from "./mappers"
import type { ArtifactUploadResponse } from "./types"

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
  list: async (
    convId: number,
    params?: { run_id?: number; kind?: string; limit?: number }
  ) =>
    (
      await sdk.artifactsList({
        conversationId: convId,
        runId: params?.run_id ?? null,
        kind: params?.kind ?? null,
        includeDeleted: false,
        limit: params?.limit ?? 100,
      })
    ).map(toArtifact),

  /** Get artifact detail (includes extracted_text and version chain). */
  get: async (convId: number, artifactId: number) =>
    toArtifactDetail(
      await sdk.artifactsGet({ conversationId: convId, artifactId })
    ),

  /** Download URL for an artifact's raw file content. */
  downloadUrl: (convId: number, artifactId: number) =>
    `/api/conversations/${convId}/artifacts/${artifactId}/download`,

  /** Soft-delete an artifact. */
  delete: async (_convId: number, artifactId: number) =>
    sdk.artifactsDelete({ idempotencyKey: idempotencyKey(), id: artifactId }),
}
