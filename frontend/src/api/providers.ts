import { api } from "./client"
import type { Provider, ProviderCreate, ProviderUpdate } from "./types"

export const providersApi = {
  list: () => api.get<Provider[]>("/api/providers"),

  create: (body: ProviderCreate) =>
    api.post<Provider>("/api/providers", body),

  get: (id: number) => api.get<Provider>(`/api/providers/${id}`),

  update: (id: number, body: ProviderUpdate) =>
    api.patch<Provider>(`/api/providers/${id}`, body),

  delete: (id: number) =>
    api.delete<{ deleted: number }>(`/api/providers/${id}`),
}
