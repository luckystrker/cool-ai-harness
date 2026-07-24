import { api } from "./client"
import type {
  ModelInfo,
  ModelsPreviewRequest,
  Provider,
  ProviderCreate,
  ProviderUpdate,
} from "./types"

export const providersApi = {
  list: () => api.get<Provider[]>("/api/providers"),

  create: (body: ProviderCreate) =>
    api.post<Provider>("/api/providers", body),

  get: (id: number) => api.get<Provider>(`/api/providers/${id}`),

  update: (id: number, body: ProviderUpdate) =>
    api.patch<Provider>(`/api/providers/${id}`, body),

  delete: (id: number) =>
    api.delete<{ deleted: number }>(`/api/providers/${id}`),

  /** Models served by an already-saved provider (edit form / chat picker). */
  listModels: (id: number) =>
    api.get<ModelInfo[]>(`/api/providers/${id}/models`),

  /** Live model-list probe for an unsaved provider (create form). */
  previewModels: (body: ModelsPreviewRequest) =>
    api.post<ModelInfo[]>("/api/providers/models/preview", body),
}
