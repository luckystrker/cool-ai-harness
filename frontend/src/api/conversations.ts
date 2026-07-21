import { api } from "./client"
import type {
  Conversation,
  ConversationCreate,
  ConversationDetail,
} from "./types"

export const conversationsApi = {
  list: () => api.get<Conversation[]>("/api/conversations"),

  create: (body: ConversationCreate = {}) =>
    api.post<Conversation>("/api/conversations", body),

  get: (id: number) =>
    api.get<ConversationDetail>(`/api/conversations/${id}`),

  delete: (id: number) =>
    api.delete<{ deleted: number }>(`/api/conversations/${id}`),
}
