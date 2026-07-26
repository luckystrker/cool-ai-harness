import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { MessageSquare, Sparkles, Paperclip } from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import { artifactsApi } from "@/api/artifacts"
import { plansApi } from "@/api/plans"
import { providersApi } from "@/api/providers"
import { settingsApi } from "@/api/settings"
import type { Message, ToolPermissions } from "@/api/types"
import { MessageBubble, type MessageViewModel } from "@/components/chat/MessageBubble"
import { ArtifactPanel } from "@/components/chat/ArtifactPanel"
import { ChatComposer } from "@/components/chat/ChatComposer"
import { ComposerToolbar } from "@/components/chat/ComposerToolbar"
import { BudgetIndicator } from "@/components/chat/BudgetIndicator"
import { useConversationStream } from "@/hooks/useConversationStream"
import {
  MODE_PRESETS,
  modeFromPerms,
  saveLastModel,
  type PermissionMode,
} from "@/lib/agentConfig"
import { getProjectForConversation } from "@/lib/projects"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

export function ChatPage() {
  const { conversationId } = useParams()
  const convId = conversationId ? Number(conversationId) : null
  const queryClient = useQueryClient()
  const scrollRef = useRef<HTMLDivElement>(null)

  const { data: detail, isLoading } = useQuery({
    queryKey: ["conversation", convId],
    queryFn: () => (convId ? conversationsApi.get(convId) : null),
    enabled: convId !== null,
  })

  // Providers feed the "suggested models" list (their default_model values)
  // and tell us which provider is active (first active, non-fallback row) so we
  // can load its live /models list for the model picker + context-window badge.
  const { data: providers = [] } = useQuery({
    queryKey: ["providers"],
    queryFn: providersApi.list,
  })

  const activeProviderId = useMemo(() => {
    const active = providers.filter((p) => p.is_active && !p.is_fallback)
    const pool = active.length ? active : providers.filter((p) => p.is_active)
    return pool[0]?.id ?? null
  }, [providers])

  const { data: providerModels = [] } = useQuery({
    queryKey: ["provider-models", activeProviderId],
    queryFn: () => providersApi.listModels(activeProviderId!),
    enabled: activeProviderId != null,
    retry: false,
    staleTime: 5 * 60_000,
  })

  // The default system prompt, fetched once so project-specific instructions
  // can be appended to it (rather than replacing it) on each outgoing message.
  const { data: systemPromptData } = useQuery({
    queryKey: ["system-prompt"],
    queryFn: settingsApi.getSystemPrompt,
    staleTime: 10 * 60_000,
  })

  const {
    pendingMsgs,
    setPendingMsgs,
    isStreaming,
    stream,
    cancel,
    clearPending,
    respondApproval,
  } = useConversationStream()

  const [artifactsOpen, setArtifactsOpen] = useState(false)
  const [pendingFiles, setPendingFiles] = useState<File[]>([])
  const [planMode, setPlanMode] = useState(false)

  // When a different conversation is selected, drop any pending bubbles.
  useEffect(() => {
    clearPending()
  }, [convId, clearPending])

  const historyMsgs = useMemo<MessageViewModel[]>(() => {
    if (!detail?.messages) return []
    return stitchHistory(detail.messages)
  }, [detail])

  // The chat model picker shows every model the user marked as available in
  // provider settings (provider.chat_models), deduped across providers.
  // Declared above the early return so the hook order is stable regardless of
  // whether convId is set.
  const suggestedModels = useMemo(
    () =>
      Array.from(
        new Set(
          providers.flatMap((p) => p.chat_models ?? []).filter((m): m is string =>
            Boolean(m && m.trim())
          )
        )
      ),
    [providers]
  )

  // Auto-scroll on any new content.
  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [historyMsgs, pendingMsgs])

  const updateMutation = useMutation({
    mutationFn: (vars: { id: number; model: string }) =>
      conversationsApi.update(vars.id, { model: vars.model }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
    },
    onError: (e) => toast.error("Failed to change model", { description: String(e) }),
  })

  const workdirMutation = useMutation({
    mutationFn: (vars: { id: number; working_directory: string }) =>
      conversationsApi.update(vars.id, { working_directory: vars.working_directory }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
      queryClient.invalidateQueries({ queryKey: ["workspace-recent"] })
    },
    onError: (e) => toast.error("Failed to change working directory", { description: String(e) }),
  })

  const modeMutation = useMutation({
    mutationFn: (vars: { id: number; permissions: ToolPermissions }) =>
      conversationsApi.update(vars.id, { permissions: vars.permissions }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    },
    onError: (e) => toast.error("Failed to change agent mode", { description: String(e) }),
  })

  const handleModelChange = (model: string) => {
    if (!convId || !model.trim()) return
    // Remember the choice so the next chat the user creates starts on it too.
    saveLastModel(model)
    updateMutation.mutate({ id: convId, model: model.trim() })
  }

  const handleWorkdirChange = (dir: string) => {
    if (!convId || !dir.trim()) return
    workdirMutation.mutate({ id: convId, working_directory: dir.trim() })
  }

  const handleModeChange = (mode: PermissionMode) => {
    if (!convId) return
    modeMutation.mutate({ id: convId, permissions: { ...MODE_PRESETS[mode] } })
  }

  const handleSend = async (content: string) => {
    if (!convId) return
    // Upload any pending file attachments first.
    if (pendingFiles.length > 0) {
      try {
        for (const file of pendingFiles) {
          await artifactsApi.upload(convId, file)
        }
        queryClient.invalidateQueries({ queryKey: ["artifacts", convId] })
        toast.success(`${pendingFiles.length} file(s) attached`)
      } catch (e) {
        toast.error("Upload failed", { description: String(e) })
      }
      setPendingFiles([])
    }
    // Pass the conversation's current model as a per-message override so a
    // freshly-picked model applies immediately without a round-trip.
    // If this chat belongs to a project with extra instructions, append them
    // to the default system prompt so the agent carries the project context.
    const project = getProjectForConversation(convId)
    let systemPrompt: string | undefined
    if (project?.systemInstructions) {
      const base = systemPromptData?.prompt ?? ""
      systemPrompt = base
        ? `${base}\n\n# Project instructions\n${project.systemInstructions}`
        : project.systemInstructions
    }
    const errored = await stream(
      convId,
      content,
      detail?.model || undefined,
      planMode,
      systemPrompt
    )
    // Reset plan mode after sending (one-shot toggle).
    const wasPlanMode = planMode
    setPlanMode(false)
    // Persisted history is now the source of truth — refetch and drop pending
    // only after the fresh data is in the cache (avoids a blank flash between
    // the stream ending and the history arriving). On a failed turn, keep the
    // assistant error bubble so the user sees why there was no reply.
    // When plan mode was active, keep the pending PlanCard visible so the user
    // can approve/reject the plan.
    await queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    queryClient.invalidateQueries({ queryKey: ["conversations"] })
    await queryClient.refetchQueries({ queryKey: ["conversation", convId] })
    if (!errored && !wasPlanMode) clearPending()
  }

  const handleAttach = (files: File[]) => {
    setPendingFiles((prev) => [...prev, ...files])
    // Open the artifacts panel so the user sees the context.
    setArtifactsOpen(true)
  }

  const handleRemoveFile = (index: number) => {
    setPendingFiles((prev) => prev.filter((_, i) => i !== index))
  }

  // --- Plan Mode handlers (Фаза 2 §1) ---

  const handlePlanExecute = useCallback(async () => {
    if (!convId) return
    const planMsg = pendingMsgs.find((m) => m.plan)
    if (!planMsg?.plan) return
    const planId = planMsg.plan.id
    try {
      // Stream the plan execution via SSE.
      const resp = await fetch(plansApi.executeUrl(convId, planId), {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "text/event-stream" },
      })
      if (!resp.ok || !resp.body) {
        throw new Error(`Execution failed (${resp.status})`)
      }
      // Read the SSE stream and update the plan card in pending messages.
      const reader = resp.body.getReader()
      const decoder = new TextDecoder()
      let buffer = ""
      while (true) {
        const { done, value } = await reader.read()
        if (done) break
        buffer += decoder.decode(value, { stream: true })
        // Parse SSE frames.
        let sepIdx: number
        while ((sepIdx = buffer.indexOf("\n\n")) !== -1) {
          const frame = buffer.slice(0, sepIdx)
          buffer = buffer.slice(sepIdx + 2)
          const dataLine = frame.split("\n").find((l) => l.startsWith("data:"))
          if (!dataLine) continue
          try {
            const parsed = JSON.parse(dataLine.slice(5).trim())
            const payload = parsed?.payload ?? parsed
            const kind = parsed?.kind ?? ""
            // Update the plan in pending messages based on events.
            if (kind === "plan_step_start" || kind === "plan_step_complete" || kind === "plan_progress") {
              setPendingMsgs((cur) =>
                cur.map((m) => {
                  if (!m.plan) return m
                  const steps = m.plan.steps.map((s) => {
                    if (kind === "plan_step_start" && s.position === payload.position) {
                      return { ...s, status: "running" as const }
                    }
                    if (kind === "plan_step_complete" && s.position === payload.position) {
                      return { ...s, status: (payload.status ?? "completed") as typeof s.status, result_summary: payload.result_summary ?? s.result_summary }
                    }
                    return s
                  })
                  const allDone = steps.every((s) => ["completed", "failed", "skipped"].includes(s.status))
                  const hasFailed = steps.some((s) => s.status === "failed")
                  return {
                    ...m,
                    plan: { ...m.plan, steps, status: allDone ? (hasFailed ? "failed" as const : "completed" as const) : "executing" as const },
                  }
                })
              )
            }
          } catch { /* skip malformed frames */ }
        }
      }
      // Mark execution done.
      queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    } catch (e) {
      toast.error("Plan execution failed", { description: String(e) })
    }
  }, [convId, pendingMsgs, queryClient, setPendingMsgs])

  const handlePlanApprove = useCallback(
    async (approved: boolean) => {
      if (!convId) return
      // Find the plan from the pending messages.
      const planMsg = pendingMsgs.find((m) => m.plan)
      if (!planMsg?.plan) return
      const planId = planMsg.plan.id
      try {
        await plansApi.approve(convId, planId, approved)
        if (approved) {
          toast.success("Plan approved — starting execution…")
          // Auto-execute after approval.
          await handlePlanExecute()
        } else {
          toast.info("Plan rejected")
          clearPending()
        }
      } catch (e) {
        toast.error("Plan action failed", { description: String(e) })
      }
    },
    [convId, pendingMsgs, clearPending, handlePlanExecute]
  )

  // Conversation context usage = the prompt_tokens of the most recent assistant
  // turn. That usage is cumulative for the whole run, so its prompt_tokens
  // already reflect the full conversation context sent to the model — no need
  // (and no correctness) in summing across messages. Declared above the early
  // return so the hook order is stable regardless of whether convId is set.
  const usedContextTokens = useMemo(() => {
    const msgs = detail?.messages
    if (!msgs) return null
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i]
      if (m.role === "assistant" && m.usage) {
        const pt = (m.usage as { prompt_tokens?: number }).prompt_tokens
        if (typeof pt === "number" && pt > 0) return pt
      }
    }
    return null
  }, [detail])

  if (!convId) return <EmptyState />

  const currentModel = detail?.model || ""

  return (
    <div className="flex h-full flex-col">
      <header className="flex h-14 items-center gap-2 border-b px-4">
        <MessageSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
        <span className="truncate font-medium">
          {detail?.title || `Conversation #${convId}`}
        </span>
        <Button
          variant="ghost"
          size="sm"
          className={cn("ml-auto px-2", artifactsOpen ? "text-foreground" : "text-muted-foreground")}
          title="Toggle attachments panel"
          onClick={() => setArtifactsOpen((v) => !v)}
        >
          <Paperclip className="h-4 w-4" />
        </Button>
        <BudgetIndicator />
      </header>

      <div className="flex flex-1 overflow-hidden">
        <div className="flex flex-1 flex-col overflow-hidden">
          <div ref={scrollRef} className="flex-1 overflow-y-auto">
            <div className="mx-auto max-w-3xl py-4">
              {isLoading ? (
                <div className="py-16 text-center text-sm text-muted-foreground">
                  Loading…
                </div>
              ) : historyMsgs.length === 0 && pendingMsgs.length === 0 ? (
                <div className="py-16 text-center text-sm text-muted-foreground">
                  Send a message to start the conversation.
                </div>
              ) : (
                <>
                  {historyMsgs.map((m) => (
                    <MessageBubble key={m.id} msg={m} />
                  ))}
                  {pendingMsgs.map((m) => (
                    <MessageBubble
                      key={m.id}
                      msg={m}
                      onRespondApproval={respondApproval}
                      onPlanApprove={handlePlanApprove}
                      onPlanExecute={handlePlanExecute}
                    />
                  ))}
                </>
              )}
            </div>
          </div>

          <div className="mx-auto w-full max-w-3xl">
            <ChatComposer
              onSend={handleSend}
              onCancel={cancel}
              onAttach={handleAttach}
              streaming={isStreaming}
              pendingFiles={pendingFiles}
              onRemoveFile={handleRemoveFile}
              toolbar={
                <ComposerToolbar
                  workingDirectory={detail?.working_directory ?? null}
                  onWorkingDirectoryChange={handleWorkdirChange}
                  mode={modeFromPerms((detail?.permissions as ToolPermissions | null) ?? {})}
                  onModeChange={handleModeChange}
                  currentModel={currentModel}
                  modelOptions={providerModels}
                  suggestedModels={suggestedModels}
                  usedContextTokens={usedContextTokens}
                  onModelChange={handleModelChange}
                  modelPending={updateMutation.isPending}
                  planMode={planMode}
                  onPlanModeChange={setPlanMode}
                />
              }
            />
          </div>
        </div>

        {artifactsOpen && (
          <div className="w-72 shrink-0">
            <ArtifactPanel conversationId={convId} />
          </div>
        )}
      </div>
    </div>
  )
}

// --- helpers ---

/**
 * Convert persisted messages into view models, stitching each role="tool"
 * result back onto the tool call that produced it (matched by tool_call_id).
 *
 * Tool-role rows would otherwise render as empty bubbles (MessageBubble
 * returns null for them), so they are dropped once their result has been
 * attached to the originating assistant tool call.
 */
function stitchHistory(messages: Message[]): MessageViewModel[] {
  // Build a lookup: tool_call_id -> tool result block props.
  const resultsByCallId = new Map<string, Message>()
  for (const m of messages) {
    if (m.role === "tool" && m.tool_result?.tool_call_id) {
      resultsByCallId.set(m.tool_result.tool_call_id, m)
    }
  }

  const out: MessageViewModel[] = []
  for (const m of messages) {
    if (m.role === "tool") continue // results are inlined into the assistant bubble

    const toolCalls =
      m.tool_calls?.map((tc, i) => {
        const id = tc.id ?? undefined
        const toolRow = id ? resultsByCallId.get(id) : undefined
        const result = toolRow?.tool_result?.result
        return {
          key: `${m.id}-tc-${i}`,
          call: {
            id,
            type: tc.type,
            name: tc.name,
            // arguments can be null in stored rows (older data); coerce so the
            // ToolCallBlock renderer never hits Object.keys(undefined).
            arguments: tc.arguments ?? {},
          },
          pending: false,
          ...(result ? { result } : {}),
        }
      }) ?? undefined

    out.push({
      id: `db-${m.id}`,
      role: m.role,
      content: m.content ?? "",
      toolCalls,
      thinking: m.thinking ?? undefined,
      usage: (m.usage as MessageViewModel["usage"]) ?? undefined,
      model: m.model ?? undefined,
      createdAt: m.created_at,
      // Persisted turn duration becomes elapsedMs so the footnote (and the
      // thinking block) render the same way for history as for the live stream.
      elapsedMs: m.duration_ms ?? undefined,
    })
  }
  return out
}

function EmptyState() {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 p-8 text-center">
      <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-primary text-primary-foreground">
        <Sparkles className="h-6 w-6" />
      </div>
      <div>
        <h1 className="text-xl font-semibold">Cool AI Harness</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          Select a conversation on the left, or create a new one to get started.
        </p>
      </div>
    </div>
  )
}
