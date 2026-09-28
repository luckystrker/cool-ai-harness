import type { ContentPart, EventEnvelope } from "./generated/cool_protocol"
import { coolApiBaseUrl, coolApiToken, idempotencyKey, sdk, streamRunEvents } from "./sdk"
import type { SendMessageRequest } from "./types"

/**
 * Stream one canonical agent turn.
 *
 * `session.for_conversation` binds the legacy conversation to its durable Rust
 * session (importing the legacy transcript once), `session.prompt` starts a run,
 * and `streamRunEvents` follows the run on the canonical cursor/reconnect
 * stream. Each yielded envelope carries one canonical event plus its durable
 * `seq`, which the reducer uses to reject gaps or duplicates.
 *
 * Attachments ride the canonical prompt as `artifact` content parts — the app
 * server expands them (text extracts inline; images as a marker note).
 */
export async function* streamConversationMessage(
  conversationId: number,
  body: SendMessageRequest,
  signal?: AbortSignal
): AsyncGenerator<EventEnvelope> {
  const link = await sdk.sessionForConversation({
    idempotencyKey: `session-for-conversation-${conversationId}`,
    conversationId,
  })
  const content: ContentPart[] = [{ type: "text", text: body.content }]
  for (const artifactId of body.artifact_ids ?? []) {
    content.push({ type: "artifact", artifactId: String(artifactId) })
  }
  const accepted = await sdk.sessionPrompt({
    idempotencyKey: idempotencyKey(),
    sessionId: link.sessionId,
    content,
    model: body.model ?? null,
    planMode: body.plan_mode ?? false,
    longTaskMode: body.long_task_mode ?? false,
    systemPrompt: body.system_prompt ?? null,
  })
  yield* streamRunEvents(accepted.runId, {
    baseUrl: coolApiBaseUrl,
    signal,
    ...(coolApiToken ? { token: coolApiToken } : {}),
  })
}
