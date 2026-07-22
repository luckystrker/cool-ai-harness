import { useEffect, useMemo, useRef, useState } from "react"
import { useParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { MessageSquare, Sparkles, ChevronDown, Check, Pencil, Settings2 } from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import { providersApi } from "@/api/providers"
import type { Message, ToolPermissions } from "@/api/types"
import { MessageBubble, type MessageViewModel } from "@/components/chat/MessageBubble"
import { ApprovalDialog } from "@/components/chat/ApprovalDialog"
import { ChatComposer } from "@/components/chat/ChatComposer"
import { ConversationSettingsDialog } from "@/components/chat/ConversationSettingsDialog"
import { useConversationStream } from "@/hooks/useConversationStream"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
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

  // Providers feed the "suggested models" list (their default_model values).
  const { data: providers = [] } = useQuery({
    queryKey: ["providers"],
    queryFn: providersApi.list,
  })

  const {
    pendingMsgs,
    isStreaming,
    stream,
    cancel,
    clearPending,
    pendingApproval,
    respondApproval,
  } = useConversationStream()

  const [settingsOpen, setSettingsOpen] = useState(false)

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

  const handleModelChange = (model: string) => {
    if (!convId || !model.trim()) return
    updateMutation.mutate({ id: convId, model: model.trim() })
  }

  const handleSend = async (content: string) => {
    if (!convId) return
    // Pass the conversation's current model as a per-message override so a
    // freshly-picked model applies immediately without a round-trip.
    await stream(convId, content, detail?.model || undefined)
    // Persisted history is now the source of truth — refetch and drop pending.
    await queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    queryClient.invalidateQueries({ queryKey: ["conversations"] })
    clearPending()
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
        <ModelPicker
          currentModel={currentModel}
          suggestedModels={suggestedModels}
          onChange={handleModelChange}
          pending={updateMutation.isPending}
        />
        <Button
          variant="ghost"
          size="sm"
          className="px-2 text-muted-foreground"
          title="Conversation settings (working directory & permissions)"
          onClick={() => setSettingsOpen(true)}
        >
          <Settings2 className="h-4 w-4" />
        </Button>
      </header>

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
                <MessageBubble key={m.id} msg={m} />
              ))}
            </>
          )}
        </div>
      </div>

      <div className="mx-auto w-full max-w-3xl">
        <ChatComposer
          onSend={handleSend}
          onCancel={cancel}
          streaming={isStreaming}
        />
      </div>

      <ApprovalDialog approval={pendingApproval} onRespond={respondApproval} />
      <ConversationSettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        conversationId={convId}
        workingDirectory={detail?.working_directory ?? null}
        permissions={(detail?.permissions as ToolPermissions | null) ?? null}
        onSaved={() => {
          queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
        }}
      />
    </div>
  )
}

/**
 * Inline model picker for the chat header.
 *
 * Shows the conversation's current model as a button; clicking it opens a
 * dropdown of provider default_model values plus a "Custom…" entry that lets
 * the user type any model identifier.
 */
function ModelPicker({
  currentModel,
  suggestedModels,
  onChange,
  pending,
}: {
  currentModel: string
  suggestedModels: string[]
  onChange: (model: string) => void
  pending: boolean
}) {
  const [customOpen, setCustomOpen] = useState(false)
  const [customValue, setCustomValue] = useState("")

  const selectModel = (model: string) => {
    onChange(model)
  }

  const submitCustom = () => {
    const v = customValue.trim()
    if (!v) return
    onChange(v)
    setCustomOpen(false)
    setCustomValue("")
  }

  return (
    <DropdownMenu onOpenChange={(open) => { if (!open) setCustomOpen(false) }}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="ml-auto gap-1.5 px-2 text-xs font-normal text-muted-foreground"
          title="Change model"
        >
          {pending ? (
            <span className="text-muted-foreground">saving…</span>
          ) : currentModel ? (
            <span className="font-mono">{currentModel}</span>
          ) : (
            <span>Set model</span>
          )}
          <ChevronDown className="h-3 w-3" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        {currentModel && (
          <>
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Current
            </DropdownMenuLabel>
            <DropdownMenuItem
              className="font-mono text-xs"
              onSelect={(e) => {
                e.preventDefault()
              }}
            >
              <Check className="h-3.5 w-3.5" /> {currentModel}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        )}

        {suggestedModels.length > 0 && (
          <>
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Provider models
            </DropdownMenuLabel>
            {suggestedModels.map((m) => (
              <DropdownMenuItem
                key={m}
                className="font-mono text-xs"
                onSelect={(e) => {
                  e.preventDefault()
                  selectModel(m)
                }}
              >
                {m === currentModel ? (
                  <Check className="h-3.5 w-3.5" />
                ) : (
                  <span className="inline-block h-3.5 w-3.5" />
                )}
                {m}
              </DropdownMenuItem>
            ))}
            <DropdownMenuSeparator />
          </>
        )}

        {customOpen ? (
          <form
            className="flex items-center gap-1 p-1"
            onSubmit={(e) => {
              e.preventDefault()
              submitCustom()
            }}
          >
            <Input
              autoFocus
              placeholder="model name"
              value={customValue}
              onChange={(e) => setCustomValue(e.target.value)}
              className="h-8 font-mono text-xs"
            />
            <Button type="submit" size="sm" className="h-8 px-2">
              Set
            </Button>
          </form>
        ) : (
          <DropdownMenuItem
            className={cn("text-xs", !suggestedModels.length && !currentModel && "")}
            onSelect={(e) => {
              e.preventDefault()
              setCustomOpen(true)
            }}
          >
            <Pencil className="h-3.5 w-3.5" /> Custom model…
          </DropdownMenuItem>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
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
          call: { id, type: tc.type, name: tc.name, arguments: tc.arguments },
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
