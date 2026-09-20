import type { EventEnvelope, JsonValue } from "./generated/cool_protocol"
import { coolApiBaseUrl, coolApiToken, idempotencyKey, sdk, streamRunEvents } from "./sdk"
import type { AgentEvent, SendMessageRequest } from "./types"

/**
 * Stream one canonical agent turn.
 *
 * `session.for_conversation` binds the legacy conversation to its durable Rust
 * session (importing the legacy transcript once), `session.prompt` starts a run,
 * and `streamRunEvents` follows the run on the canonical cursor/reconnect
 * stream. Each yielded envelope carries one canonical event plus its durable
 * `seq`, which the reducer uses to reject gaps or duplicates.
 *
 * Attachments: `session.prompt` currently accepts only text `ContentPart`s, so
 * a send that carries `artifact_ids` still uses the legacy Python SSE endpoint
 * (see `streamConversationMessageLegacy`) until artifact input is expressible
 * in the canonical protocol.
 */
export async function* streamConversationMessage(
  conversationId: number,
  body: SendMessageRequest,
  signal?: AbortSignal
): AsyncGenerator<EventEnvelope> {
  if (body.artifact_ids?.length) {
    yield* streamConversationMessageLegacy(conversationId, body, signal)
    return
  }
  const link = await sdk.sessionForConversation({
    idempotencyKey: `session-for-conversation-${conversationId}`,
    conversationId,
  })
  const accepted = await sdk.sessionPrompt({
    idempotencyKey: idempotencyKey(),
    sessionId: link.sessionId,
    content: [{ type: "text", text: body.content }],
    model: body.model ?? null,
    planMode: body.plan_mode ?? false,
    systemPrompt: body.system_prompt ?? null,
  })
  yield* streamRunEvents(accepted.runId, {
    baseUrl: coolApiBaseUrl,
    signal,
    ...(coolApiToken ? { token: coolApiToken } : {}),
  })
}

/**
 * Legacy per-turn POST SSE (`POST /api/conversations/{id}/messages`).
 *
 * Kept only as the attachment fallback for the Python runtime; the canonical
 * surfaces (text chat, plans without attachments) use the Rust facade.
 */
async function* streamConversationMessageLegacy(
  conversationId: number,
  body: SendMessageRequest,
  signal?: AbortSignal
): AsyncGenerator<EventEnvelope> {
  const resp = await fetch(`/api/conversations/${conversationId}/messages`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "text/event-stream" },
    body: JSON.stringify(body),
    signal,
  })

  if (!resp.ok || !resp.body) {
    let detail: unknown
    try {
      detail = await resp.json()
    } catch {
      detail = await resp.text().catch(() => undefined)
    }
    throw new Error(`Stream failed (${resp.status}): ${JSON.stringify(detail)}`)
  }

  const reader = resp.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ""

  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      let sepIndex: number
      while ((sepIndex = findFrameEnd(buffer)) !== -1) {
        const sepLen = buffer.startsWith("\r\n\r\n", sepIndex) ? 4 : 2
        const rawEvent = buffer.slice(0, sepIndex)
        buffer = buffer.slice(sepIndex + sepLen)
        const parsed = parseLegacyEvent(rawEvent)
        if (parsed) yield* legacyToCanonical(parsed)
      }
    }
    if (buffer.trim()) {
      const parsed = parseLegacyEvent(buffer)
      if (parsed) yield* legacyToCanonical(parsed)
    }
  } finally {
    reader.releaseLock()
  }
}

function findFrameEnd(buffer: string): number {
  const crlf = buffer.indexOf("\r\n\r\n")
  const lf = buffer.indexOf("\n\n")
  if (crlf === -1) return lf
  if (lf === -1) return crlf
  return Math.min(crlf, lf)
}

/** Parse one legacy SSE frame ("event: <kind>\ndata: <json>") into an AgentEvent. */
function parseLegacyEvent(raw: string): AgentEvent | null {
  let kind = "message"
  const dataLines: string[] = []
  for (const line of raw.split("\n")) {
    const trimmed = line.endsWith("\r") ? line.slice(0, -1) : line
    if (trimmed.startsWith("event:")) {
      kind = trimmed.slice(6).trim()
    } else if (trimmed.startsWith("data:")) {
      dataLines.push(trimmed.slice(5).trim())
    }
  }
  if (dataLines.length === 0) return null
  try {
    const parsed = JSON.parse(dataLines.join("\n"))
    const payload =
      parsed && typeof parsed === "object" && "payload" in parsed
        ? parsed.payload ?? {}
        : parsed
    const eventKind =
      parsed && typeof parsed === "object" && "kind" in parsed && parsed.kind
        ? (parsed.kind as string)
        : kind
    return { kind: eventKind as AgentEvent["kind"], payload }
  } catch {
    return null
  }
}

/**
 * Project legacy `AgentEvent`s into canonical `EventEnvelope`s so the hook's
 * single canonical reducer can consume both paths. The fallback is only used
 * for attachment sends, where the Python runtime remains the source.
 */
function legacyToCanonical(event: AgentEvent): EventEnvelope[] {
  const payload = event.payload as Record<string, unknown>
  const text = typeof payload.text === "string" ? payload.text : ""
  const runId = `legacy-run-${String(payload.run_id ?? "unknown")}`
  const base = {
    eventId: `legacy-${event.kind}-${String(payload.id ?? payload.approval_id ?? text.length)}`,
    schemaVersion: 1 as const,
    sessionId: "legacy",
    runId,
    itemId: null,
    seq: 0,
    occurredAt: new Date().toISOString(),
    actor: { id: "local-user", kind: "local_user" as const },
    source: "python",
    causationId: null,
    correlationId: null,
  }
  const args = (payload.arguments as { [key: string]: JsonValue }) ?? {}
  switch (event.kind) {
    case "thinking":
    case "react_thought":
      return [{ ...base, event: { kind: "reasoning.delta", payload: { text, channel: "analysis" } } }]
    case "token":
      return [{ ...base, event: { kind: "content.delta", payload: { text, channel: "final" } } }]
    case "tool_call_start":
      return [{
        ...base,
        event: {
          kind: "tool.requested",
          payload: { callId: String(payload.id ?? "tool"), name: String(payload.name ?? "unknown"), arguments: args },
        },
      }]
    case "tool_result": {
      const callId = String(payload.id ?? "")
      const result = payload.result as Record<string, unknown> | undefined
      if (result?.is_error) {
        return [{
          ...base,
          event: {
            kind: "tool.failed",
            payload: {
              callId,
              name: "tool",
              errorCode: "legacy_tool_error",
              message: (result.error as string) ?? (result.output as string) ?? null,
            },
          },
        }]
      }
      return [{
        ...base,
        event: {
          kind: "tool.completed",
          payload: { callId, name: "tool", result: (payload.result ?? null) as JsonValue },
        },
      }]
    }
    case "tool_approval_request":
      return [{
        ...base,
        event: {
          kind: "tool.approval_required",
          payload: {
            callId: String(payload.id ?? "tool"),
            name: String(payload.name ?? "unknown"),
            arguments: args,
            reason: String(payload.reason ?? ""),
            approvalId: String(payload.approval_id ?? ""),
            revision: Number(payload.revision ?? 1),
            breakpointType: (payload.breakpoint_type as string | null) ?? null,
            resultPreview: (payload.result_preview as string | null) ?? null,
            currentContent: (payload.current_content as string | null) ?? null,
          },
        },
      }]
    case "tool_approval_resolved": {
      const decision =
        payload.decision === "approved" || payload.decision === "denied" || payload.decision === "timed_out"
          ? payload.decision
          : "denied"
      return [{
        ...base,
        event: {
          kind: "tool.approval_resolved",
          payload: {
            callId: String(payload.id ?? ""),
            approvalId: String(payload.approval_id ?? ""),
            revision: Number(payload.revision ?? 1),
            decision,
          },
        },
      }]
    }
    case "budget_alert":
      return [{
        ...base,
        event: {
          kind: payload.pct !== undefined && Number(payload.pct) >= 100 ? "budget.exceeded" : "budget.warning",
          payload: {
            window: String(payload.window ?? "budget"),
            spendUsd: Number(payload.spend_usd ?? 0),
            limitUsd: Number(payload.limit_usd ?? 0),
            percent: Number(payload.pct ?? 0),
          },
        },
      }]
    case "plan_generated": {
      const planId = String(payload.plan_id ?? "")
      const steps = Array.isArray(payload.steps)
        ? (payload.steps as { position?: number; title?: string; status?: string; result_summary?: string | null }[])
        : []
      const planSteps = steps.map((step, index) => ({
        planId,
        position: step.position ?? index,
        title: step.title ?? "step",
        status: step.status ?? "pending",
        resultSummary: step.result_summary ?? null,
      }))
      const created: EventEnvelope = {
        ...base,
        event: {
          kind: "plan.created",
          payload: {
            planId,
            title: (payload.title as string | null) ?? null,
            totalSteps: planSteps.length,
            steps: planSteps,
            // Legacy plan events carry no durable store id; the app server
            // stamps it for canonical plan-mode runs.
            storePlanId: null,
          },
        },
      }
      const stepEvents = planSteps.flatMap((common): EventEnvelope[] => {
        if (common.status === "in_progress" || common.status === "running") {
          return [{ ...base, event: { kind: "plan.step_started", payload: common } }]
        }
        if (common.status === "completed" || common.status === "failed") {
          return [{ ...base, event: { kind: "plan.step_completed", payload: common } }]
        }
        return []
      })
      return [created, ...stepEvents]
    }
    case "plan_step_start":
      return [{
        ...base,
        event: {
          kind: "plan.step_started",
          payload: {
            planId: "",
            position: Number(payload.position ?? 0),
            title: String(payload.title ?? "step"),
            status: "running",
            resultSummary: null,
          },
        },
      }]
    case "plan_step_complete":
      return [{
        ...base,
        event: {
          kind: "plan.step_completed",
          payload: {
            planId: "",
            position: Number(payload.position ?? 0),
            title: String(payload.title ?? "step"),
            status: String(payload.status ?? "completed"),
            resultSummary: (payload.result_summary as string | null) ?? null,
          },
        },
      }]
    case "plan_progress":
      return [{
        ...base,
        event: {
          kind: "plan.progress",
          payload: {
            planId: "",
            completedSteps: Number(payload.completed ?? 0),
            totalSteps: Number(payload.total ?? 0),
            message: null,
            status:
              payload.status === "completed" || payload.status === "failed"
                ? payload.status
                : "executing",
          },
        },
      }]
    case "subagent_started":
    case "subagent_completed":
    case "subagent_failed": {
      const kind =
        event.kind === "subagent_started"
          ? "subagent.started"
          : event.kind === "subagent_completed"
            ? "subagent.completed"
            : "subagent.failed"
      return [{
        ...base,
        event: {
          kind,
          payload: {
            subagentRunId: String(payload.subagent_run_id ?? payload.name ?? "subagent"),
            name: (payload.name as string | null) ?? null,
            status: kind.split(".")[1],
            summary: (payload.result_summary as string | null) ?? null,
            error: (payload.error as string | null) ?? null,
          },
        },
      }]
    }
    case "subagent_progress":
      return [{
        ...base,
        event: {
          kind: "subagent.progress",
          payload: {
            subagentRunId: String(payload.subagent_run_id ?? payload.name ?? "subagent"),
            message: String(payload.message ?? ""),
            percent: (payload.percent as number | null) ?? null,
          },
        },
      }]
    case "message": {
      const calls = Array.isArray(payload.tool_calls)
        ? (payload.tool_calls as { id?: string | null; name?: string; arguments?: Record<string, unknown> }[])
        : []
      return calls.map((call) => ({
        ...base,
        eventId: `${base.eventId}-${call.id ?? call.name}`,
        event: {
          kind: "tool.requested" as const,
          payload: {
            callId: call.id ?? `tc-${call.name}`,
            name: call.name ?? "unknown",
            arguments: (call.arguments ?? {}) as { [key: string]: JsonValue },
          },
        },
      }))
    }
    case "finish": {
      const usage = payload.usage as
        | { prompt_tokens?: number; completion_tokens?: number; total_tokens?: number; cost_usd?: number | null }
        | undefined
      const events: EventEnvelope[] = [
        { ...base, event: { kind: "run.completed", payload: { reason: String(payload.reason ?? "stop"), errorCode: null } } },
      ]
      if (usage) {
        events.push({
          ...base,
          eventId: `${base.eventId}-usage`,
          event: {
            kind: "usage.updated",
            payload: {
              promptTokens: usage.prompt_tokens ?? 0,
              completionTokens: usage.completion_tokens ?? 0,
              totalTokens: usage.total_tokens ?? 0,
              costUsd: usage.cost_usd ?? null,
            },
          },
        })
      }
      return events
    }
    case "error":
      return [{
        ...base,
        event: {
          kind: "run.failed",
          payload: { reason: String(payload.message ?? "error"), errorCode: String(payload.message ?? "error") },
        },
      }]
    default:
      return [{ ...base, event: { kind: "session.updated", payload: { title: null, projectKey: null } } }]
  }
}
