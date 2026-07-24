import { useEffect, useMemo, useRef, useState } from "react"
import { useParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { MessageSquare, Sparkles, Paperclip } from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import { artifactsApi } from "@/api/artifacts"
import { providersApi } from "@/api/providers"
import type { Message, ToolPermissions } from "@/api/types"
import { MessageBubble, type MessageViewModel } from "@/components/chat/MessageBubble"
import { ArtifactPanel } from "@/components/chat/ArtifactPanel"
import { ChatComposer } from "@/components/chat/ChatComposer"
import { ComposerToolbar } from "@/components/chat/ComposerToolbar"
import { BudgetIndicator } from "@/components/chat/BudgetIndicator"
import { useConversationStream } from "@/hooks/useConversationStream"
import { MODE_PRESETS, modeFromPerms, type PermissionMode } from "@/lib/agentConfig"
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

  const {
    pendingMsgs,
    isStreaming,
    stream,
    cancel,
    clearPending,
    respondApproval,
  } = useConversationStream()

  const [artifactsOpen, setArtifactsOpen] = useState(false)
  const [pendingFiles, setPendingFiles] = useState<File[]>([])

  // When a different conversation is selected, drop any pending bubbles.
  useEffect(() => {
    clearPending()
  }, [convId, clearPending])

  const historyMsgs = useMemo<MessageViewModel[]>(() => {
    if (!detail?.messages) return []
    return stitchHistory(detail.messages)
  }, [detail])

  // Provider default_model values, deduped, feed the model picker's
  // "suggested" list. Declared above the early return so the hook order
  // is stable regardless of whether convId is set.
  const suggestedModels = useMemo(
    () =>
      Array.from(
        new Set(
          providers
            .map((p) => p.default_model)
            .filter((m): m is string => Boolean(m && m.trim()))
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
    await stream(convId, content, detail?.model || undefined)
    // Persisted history is now the source of truth — refetch and drop pending
    // only after the fresh data is in the cache (avoids a blank flash between
    // the stream ending and the history arriving).
    await queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    queryClient.invalidateQueries({ queryKey: ["conversations"] })
    await queryClient.refetchQueries({ queryKey: ["conversation", convId] })
    clearPending()
  }

  const handleAttach = (files: File[]) => {
    setPendingFiles((prev) => [...prev, ...files])
    // Open the artifacts panel so the user sees the context.
    setArtifactsOpen(true)
  }

  const handleRemoveFile = (index: number) => {
    setPendingFiles((prev) => prev.filter((_, i) => i !== index))
  }

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
                  onModelChange={handleModelChange}
                  modelPending={updateMutation.isPending}
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
