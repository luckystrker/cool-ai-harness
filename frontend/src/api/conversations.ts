import { api } from "./client"
import type {
  Conversation,
  ConversationCreate,
  ConversationDetail,
  ConversationUpdate,
} from "./types"

export const conversationsApi = {
  list: () => api.get<Conversation[]>("/api/conversations"),

  create: (body: ConversationCreate = {}) =>
    api.post<Conversation>("/api/conversations", body),

  get: (id: number) =>
    api.get<ConversationDetail>(`/api/conversations/${id}`),

  update: (id: number, body: ConversationUpdate) =>
    api.patch<Conversation>(`/api/conversations/${id}`, body),

  delete: (id: number) =>
    api.delete<{ deleted: number }>(`/api/conversations/${id}`),

  /** Resolve a pending tool-call approval (gated behind an "ask" permission). */
  approveToolCall: (convId: number, callId: string, approved: boolean) =>
    api.post<{ resolved: boolean; approved: boolean }>(
      `/api/conversations/${convId}/tool_calls/${callId}/approval`,
      { approved }
    ),
}
