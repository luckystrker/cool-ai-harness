import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useNavigate, useParams, useSearchParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  Archive,
  ArrowRight,
  CheckCircle2,
  ChevronDown,
  GitBranch,
  KeyRound,
  Loader2,
  Menu,
  MessageSquare,
  Paperclip,
  Plus,
  SearchCheck,
  ShieldCheck,
} from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { conversationsApi } from "@/api/conversations"
import { artifactsApi } from "@/api/artifacts"
import { plansApi } from "@/api/plans"
import { providersApi } from "@/api/providers"
import { settingsApi } from "@/api/settings"
import type { Message, Provider, ToolPermissions } from "@/api/types"
import { Markdown } from "@/components/chat/Markdown"
import { MessageBubble, type MessageViewModel } from "@/components/chat/MessageBubble"
import { ArtifactPanel } from "@/components/chat/ArtifactPanel"
import { ChatComposer } from "@/components/chat/ChatComposer"
import { ComposerSheet } from "@/components/chat/ComposerSheet"
import { ComposerToolbar } from "@/components/chat/ComposerToolbar"
import { BudgetIndicator } from "@/components/chat/BudgetIndicator"
import { ProfileSwitcher } from "@/components/chat/ProfileSwitcher"
import { useConversationStream } from "@/hooks/useConversationStream"
import { useIsMobile } from "@/hooks/useMediaQuery"
import { useMobileNav } from "@/hooks/useMobileNav"
import {
  MODE_PRESETS,
  loadAgentDefaults,
  loadLastModel,
  modeFromPerms,
  saveLastModel,
  type PermissionMode,
} from "@/lib/agentConfig"
import { getProjectForConversation } from "@/lib/projects"
import { Button } from "@/components/ui/button"
import { QueryErrorState } from "@/components/ui/query-state"
import { cn } from "@/lib/utils"

export function ChatPage() {
  const { conversationId } = useParams()
  const [searchParams] = useSearchParams()
  const convId = conversationId ? Number(conversationId) : null
  const queryClient = useQueryClient()
  const scrollRef = useRef<HTMLDivElement>(null)

  const { data: detail, isLoading, isError, refetch } = useQuery({
    queryKey: ["conversation", convId],
    queryFn: () => (convId ? conversationsApi.get(convId) : null),
    enabled: convId !== null,
  })

  // Providers feed the "suggested models" list (their default_model values)
  // and tell us which provider is active (first active, non-fallback row) so we
  // can load its live /models list for the model picker + context-window badge.
  const {
    data: providers = [],
    isLoading: providersLoading,
    isError: providersError,
  } = useQuery({
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
  const [sheetOpen, setSheetOpen] = useState(false)
  const isMobile = useIsMobile()
  const { openDrawer } = useMobileNav()

  // When a different conversation is selected, drop any pending bubbles.
  useEffect(() => {
    clearPending()
  }, [convId, clearPending])

  // Compaction: messages covered by the working-memory rolling summary are
  // collapsed into a summary block (expandable); the rest renders normally.
  const compactCutoff = detail?.compact_up_to_message_id ?? null
  const compactSummary = detail?.compact_summary ?? null

  const compactedMsgs = useMemo<MessageViewModel[]>(() => {
    if (!detail?.messages || compactCutoff == null || !compactSummary) return []
    return stitchHistory(detail.messages.filter((m) => m.id <= compactCutoff))
  }, [detail, compactCutoff, compactSummary])

  const historyMsgs = useMemo<MessageViewModel[]>(() => {
    if (!detail?.messages) return []
    const visible =
      compactCutoff != null && compactSummary
        ? detail.messages.filter((m) => m.id > compactCutoff)
        : detail.messages
    return stitchHistory(visible)
  }, [detail, compactCutoff, compactSummary])

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
    onError: (error) =>
      toast.error("Model was not changed", {
        description: getErrorDescription(error, "Choose a model and try again."),
      }),
  })

  const workdirMutation = useMutation({
    mutationFn: (vars: { id: number; working_directory: string }) =>
      conversationsApi.update(vars.id, { working_directory: vars.working_directory }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
      queryClient.invalidateQueries({ queryKey: ["workspace-recent"] })
    },
    onError: (error) =>
      toast.error("Working directory was not changed", {
        description: getErrorDescription(error, "Choose an accessible folder and try again."),
      }),
  })

  const modeMutation = useMutation({
    mutationFn: (vars: { id: number; permissions: ToolPermissions }) =>
      conversationsApi.update(vars.id, { permissions: vars.permissions }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    },
    onError: (error) =>
      toast.error("Agent mode was not changed", {
        description: getErrorDescription(error, "Choose a mode and try again."),
      }),
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
    const artifactIds: number[] = []
    // Upload any pending file attachments first.
    if (pendingFiles.length > 0) {
      try {
        for (const file of pendingFiles) {
          const uploaded = await artifactsApi.upload(convId, file)
          artifactIds.push(uploaded.artifact.id)
        }
        queryClient.invalidateQueries({ queryKey: ["artifacts", convId] })
        toast.success(
          pendingFiles.length === 1
            ? "1 file attached"
            : `${pendingFiles.length} files attached`
        )
      } catch (e) {
        await Promise.allSettled(
          artifactIds.map((artifactId) => artifactsApi.delete(convId, artifactId))
        )
        queryClient.invalidateQueries({ queryKey: ["artifacts", convId] })
        toast.error("Files were not attached", {
          description: getErrorDescription(e, "Check the files and try again."),
        })
        return
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
      systemPrompt,
      artifactIds
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
    const available = Math.max(0, 10 - pendingFiles.length)
    const accepted = files.slice(0, available)
    setPendingFiles((prev) => [...prev, ...accepted].slice(0, 10))
    if (accepted.length < files.length) {
      toast.warning("Up to 10 files can be attached to one message")
    }
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
      toast.error("Plan execution stopped", {
        description: getErrorDescription(e, "Review the plan status and try again."),
      })
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
        toast.error("Plan response was not saved", {
          description: getErrorDescription(e, "Try approving or rejecting the plan again."),
        })
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

  if (!convId) {
    return (
      <EmptyState
        providers={providers}
        providersLoading={providersLoading}
        providersError={providersError}
      />
    )
  }

  const currentModel = detail?.model || ""

  return (
    <div className="flex h-full flex-col">
      <header className="flex h-14 items-center gap-2 border-b px-3 md:px-4">
        <Button
          variant="ghost"
          size="icon"
          className="h-11 w-11 shrink-0 text-muted-foreground md:hidden"
          title="Open conversations"
          onClick={openDrawer}
        >
          <Menu className="h-5 w-5" />
        </Button>
        <MessageSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
        <span className="truncate font-medium">
          {detail?.title || `Conversation #${convId}`}
        </span>
        <CompactButton convId={convId} disabled={isStreaming} />
        <div className="hidden sm:block">
          <ProfileSwitcher conversation={detail ?? null} />
        </div>
        <Button
          variant="ghost"
          size="sm"
          className={cn("ml-auto hidden px-2 md:inline-flex", artifactsOpen ? "text-foreground" : "text-muted-foreground")}
          title={artifactsOpen ? "Close attachments" : "Open attachments"}
          aria-label={artifactsOpen ? "Close attachments" : "Open attachments"}
          onClick={() => setArtifactsOpen((v) => !v)}
        >
          <Paperclip className="h-4 w-4" />
        </Button>
        <BudgetIndicator />
      </header>

      <div className="relative flex flex-1 overflow-hidden">
        <div className="flex flex-1 flex-col overflow-hidden">
          <div ref={scrollRef} className="flex-1 overflow-y-auto">
            <div className="mx-auto max-w-3xl py-4">
              {isError ? (
                <QueryErrorState
                  title="Conversation could not be loaded"
                  description="Check that the local harness is running, then try again."
                  onRetry={() => void refetch()}
                />
              ) : isLoading ? (
                <div className="py-16 text-center text-sm text-muted-foreground">
                  Loading conversation…
                </div>
              ) : historyMsgs.length === 0 && pendingMsgs.length === 0 && compactedMsgs.length === 0 ? (
                <div className="py-16 text-center text-sm text-muted-foreground">
                  Ask a question or describe a task to start this conversation.
                </div>
              ) : (
                <>
                  {compactedMsgs.length > 0 && (
                    <CompactedHistory messages={compactedMsgs} />
                  )}
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
                  {compactedMsgs.length > 0 && compactSummary && (
                    <CompactSummary summary={compactSummary} />
                  )}
                </>
              )}
            </div>
          </div>

          <div className="mx-auto w-full max-w-3xl">
            <ChatComposer
              key={convId}
              initialValue={searchParams.get("draft") ?? ""}
              onSend={handleSend}
              onCancel={cancel}
              onAttach={handleAttach}
              streaming={isStreaming}
              pendingFiles={pendingFiles}
              onRemoveFile={handleRemoveFile}
              leading={
                isMobile ? (
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-11 w-11 shrink-0 text-muted-foreground"
                    title="Chat settings"
                    onClick={() => setSheetOpen(true)}
                  >
                    <Plus className="h-5 w-5" />
                  </Button>
                ) : undefined
              }
              toolbar={
                isMobile ? undefined : (
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
                )
              }
            />
            {isMobile && (
              <ComposerSheet
                open={sheetOpen}
                onClose={() => setSheetOpen(false)}
                workingDirectory={detail?.working_directory ?? null}
                onWorkingDirectoryChange={handleWorkdirChange}
                mode={modeFromPerms((detail?.permissions as ToolPermissions | null) ?? {})}
                onModeChange={handleModeChange}
                currentModel={currentModel}
                modelOptions={providerModels}
                suggestedModels={suggestedModels}
                onModelChange={handleModelChange}
                planMode={planMode}
                onPlanModeChange={setPlanMode}
                pendingFiles={pendingFiles}
                onAttach={handleAttach}
                onRemoveFile={handleRemoveFile}
              />
            )}
          </div>
        </div>

        {artifactsOpen && (
          <div className="absolute inset-y-0 right-0 z-30 w-72 shrink-0 shadow-xl md:static md:z-auto md:shadow-none">
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

function EmptyState({
  providers,
  providersLoading,
  providersError,
}: {
  providers: Provider[]
  providersLoading: boolean
  providersError: boolean
}) {
  const { openDrawer } = useMobileNav()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const queryClient = useQueryClient()
  const resumedDraft = searchParams.get("draft")
  const resumedDraftStarted = useRef(false)
  const activeProvider = providers.find((provider) => provider.is_active)
  const providerReady = Boolean(activeProvider)
  const createMutation = useMutation({
    mutationFn: async (draft: string | null) => {
      const defaults = loadAgentDefaults()
      const lastModel = loadLastModel()
      const conversation = await conversationsApi.create({
        ...(lastModel ? { model: lastModel } : {}),
        permissions: defaults.permissions,
        capability_policy: defaults.capabilityPolicy,
        breakpoints: defaults.breakpoints,
      })
      return { conversation, draft }
    },
    onSuccess: ({ conversation, draft }) => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      const query = draft ? `?draft=${encodeURIComponent(draft)}` : ""
      navigate(`/chat/${conversation.id}${query}`)
    },
    onError: (_error, draft) =>
      toast.error("Conversation could not be created", {
        description: "Check that the local harness is running, then try again.",
        action: { label: "Retry", onClick: () => createMutation.mutate(draft) },
      }),
  })

  useEffect(() => {
    if (!resumedDraft || !providerReady || resumedDraftStarted.current) return
    resumedDraftStarted.current = true
    createMutation.mutate(resumedDraft)
  }, [createMutation, providerReady, resumedDraft])

  const starters = [
    {
      title: "Build or repair a feature",
      hint: "Inspect the project, propose a plan, then implement and verify it.",
      prompt:
        "Inspect this project and help me implement the next highest-impact improvement. Start with a concise plan, then make the changes and verify them.",
      icon: GitBranch,
    },
    {
      title: "Research a difficult question",
      hint: "Collect evidence, compare sources, and produce a cited answer.",
      prompt:
        "Research this question thoroughly. Compare reliable sources, call out uncertainty, and give me a concise evidence-backed recommendation: ",
      icon: SearchCheck,
    },
    {
      title: "Audit a risky change",
      hint: "Find failure modes, security gaps, and missing verification.",
      prompt:
        "Audit the current project for the highest-risk reliability, security, and usability gaps. Prioritize concrete findings and propose verified fixes.",
      icon: ShieldCheck,
    },
  ]

  const startWithDraft = (prompt: string) => {
    if (providerReady || providersLoading || providersError) {
      createMutation.mutate(prompt)
      return
    }
    const returnTo = `/?draft=${encodeURIComponent(prompt)}`
    const query = new URLSearchParams({ setup: "provider", returnTo })
    navigate(`/settings?${query.toString()}`)
  }

  return (
    <div className="relative h-full overflow-y-auto px-4 py-10 sm:px-8 sm:py-14">
      <Button
        variant="ghost"
        size="icon"
        className="absolute left-2 top-2 h-11 w-11 text-muted-foreground md:hidden"
        title="Open conversations"
        onClick={openDrawer}
      >
        <Menu className="h-5 w-5" />
      </Button>
      <div className="mx-auto flex w-full max-w-2xl flex-col items-center text-center">
        <div
          className="flex items-center gap-2 text-xs font-medium text-muted-foreground sm:gap-3"
          aria-label="First run steps: connect a model, choose an outcome, review and run"
        >
          {["Connect", "Choose", "Run"].map((step, index) => (
            <div key={step} className="flex items-center gap-3">
              <span className="flex items-center gap-1.5">
                <span
                  className={cn(
                    "grid h-7 w-7 place-items-center rounded-full text-[11px] font-semibold",
                    index === 0 && providerReady
                      ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
                      : index === (providerReady ? 1 : 0)
                        ? "bg-primary text-primary-foreground"
                        : "bg-muted text-muted-foreground"
                  )}
                >
                  {index === 0 && providerReady ? (
                    <CheckCircle2 className="h-3.5 w-3.5" />
                  ) : (
                    index + 1
                  )}
                </span>
                <span className="hidden sm:inline">{step}</span>
              </span>
              {index < 2 && <span className="h-px w-6 bg-border sm:w-9" aria-hidden />}
            </div>
          ))}
        </div>

        <h1 className="mt-7 max-w-xl text-balance text-3xl font-semibold tracking-[-0.025em] sm:text-4xl">
          What should Harness help you finish?
        </h1>
        <p className="mt-3 max-w-[60ch] text-pretty text-base leading-7 text-muted-foreground">
          Choose a real outcome. Harness opens an editable draft, then keeps the model, tools,
          approvals, and verification in one run.
        </p>

        {!providersLoading && !providerReady && !providersError && (
          <div className="mt-8 flex w-full flex-col gap-4 border-y py-5 text-left sm:flex-row sm:items-center">
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-muted text-foreground">
              <KeyRound className="h-5 w-5" />
            </span>
            <div className="min-w-0 flex-1">
              <h2 className="font-medium">Connect a model before your first run</h2>
              <p className="mt-1 text-sm leading-5 text-muted-foreground">
                Add an OpenAI-compatible or Anthropic connection. Your API key is encrypted at
                rest and never shown in full.
              </p>
            </div>
            <Button
              className="shrink-0"
              onClick={() => navigate("/settings?setup=provider&returnTo=%2F")}
            >
              Connect model
              <ArrowRight className="h-4 w-4" />
            </Button>
          </div>
        )}

        {providersLoading && (
          <p className="mt-7 text-sm text-muted-foreground" role="status" aria-live="polite">
            Checking your model connection…
          </p>
        )}

        {providersError && (
          <div className="mt-7 flex w-full items-center justify-between gap-4 border-y py-4 text-left">
            <p className="text-sm text-muted-foreground">
              Harness couldn’t verify your model connection. You can still prepare a draft.
            </p>
            <Button variant="outline" onClick={() => navigate("/settings")}>
              Check settings
            </Button>
          </div>
        )}

        {resumedDraft && providerReady && (
          <p className="mt-7 flex items-center gap-2 text-sm text-muted-foreground" role="status">
            <Loader2 className="h-4 w-4 animate-spin" />
            Model connected. Preparing your first draft…
          </p>
        )}

        <div className={cn("w-full space-y-2 text-left", !resumedDraft && "mt-8")}>
          {starters.map(({ title, hint, prompt, icon: Icon }) => (
            <button
              key={title}
              type="button"
              disabled={createMutation.isPending}
              onClick={() => startWithDraft(prompt)}
              className="group flex min-h-16 w-full items-center gap-4 rounded-xl border bg-background px-4 py-3 text-left transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50"
            >
              <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-muted text-foreground">
                <Icon className="h-5 w-5" />
              </span>
              <span className="min-w-0 flex-1">
                <span className="block font-medium">{title}</span>
                <span className="mt-0.5 block text-sm leading-5 text-muted-foreground">{hint}</span>
              </span>
              <ArrowRight className="h-4 w-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
            </button>
          ))}
        </div>

        <Button
          variant="ghost"
          className="mt-4"
          disabled={createMutation.isPending}
          onClick={() => createMutation.mutate(null)}
        >
          {createMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
          I’ll start with a blank conversation
        </Button>
      </div>
    </div>
  )
}

/**
 * Collapsed block of compacted messages at the top of the chat: the original
 * messages stay in history and can be expanded; the rolling summary itself is
 * rendered at the end of the history (see CompactSummary).
 */
function CompactedHistory({ messages }: { messages: MessageViewModel[] }) {
  const [open, setOpen] = useState(false)
  return (
    <div className="mb-4 overflow-hidden rounded-lg border border-dashed bg-muted/30">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-3 py-2 text-xs text-muted-foreground transition-colors hover:text-foreground"
        title={open ? "Hide original messages" : "Show original messages"}
      >
        <Archive className="h-3.5 w-3.5 shrink-0" />
        <span className="font-medium">
          Older messages — {messages.length} summarized
        </span>
        <ChevronDown
          className={cn(
            "ml-auto h-3.5 w-3.5 shrink-0 transition-transform",
            open && "rotate-180"
          )}
        />
      </button>
      {open && (
        <div className="space-y-3 border-t bg-background/50 px-3 py-3">
          {messages.map((m) => (
            <MessageBubble key={m.id} msg={m} />
          ))}
        </div>
      )}
    </div>
  )
}

/** Rolling summary of the compacted messages, shown at the end of the history. */
function CompactSummary({ summary }: { summary: string }) {
  return (
    <div className="mt-4 rounded-lg border border-dashed bg-muted/30 px-3 py-2">
      <div className="mb-1 flex items-center gap-2 text-xs font-medium text-muted-foreground">
        <Archive className="h-3.5 w-3.5 shrink-0" />
        Summary of older messages
      </div>
      <Markdown content={summary} className="text-sm" />
    </div>
  )
}

/** Compact button — summarizes older messages to reduce context size. */
function CompactButton({ convId, disabled }: { convId: number; disabled?: boolean }) {
  const queryClient = useQueryClient()
  const compactMutation = useMutation({
    mutationFn: () => conversationsApi.compact(convId),
    onSuccess: (data) => {
      if (data.status === "compacted") {
        toast.success(`Summarized ${data.messages_compacted} older messages`)
        // Refresh the conversation so compacted messages collapse immediately.
        queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
      } else {
        toast.info(data.reason || "Nothing to compact")
      }
    },
    onError: (error) =>
      toast.error("Conversation context was not compacted", {
        description: getErrorDescription(error, "Try compacting the conversation again."),
      }),
  })

  return (
    <Button
      variant="ghost"
      size="sm"
      className="px-2 text-muted-foreground"
      title="Summarize older messages to free context space"
      disabled={disabled || compactMutation.isPending}
      onClick={() => compactMutation.mutate()}
    >
      {compactMutation.isPending ? (
        <Loader2 className="h-4 w-4 animate-spin" />
      ) : (
        <Archive className="h-4 w-4" />
      )}
      <span className="ml-1.5 hidden sm:inline">Summarize history</span>
    </Button>
  )
}
