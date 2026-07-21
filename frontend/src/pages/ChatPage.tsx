import { useEffect, useMemo, useRef } from "react"
import { useParams } from "react-router-dom"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { MessageSquare, Sparkles } from "lucide-react"
import { conversationsApi } from "@/api/conversations"
import type { Message } from "@/api/types"
import { MessageBubble, type MessageViewModel } from "@/components/chat/MessageBubble"
import { ChatComposer } from "@/components/chat/ChatComposer"
import { useConversationStream } from "@/hooks/useConversationStream"

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

  const { pendingMsgs, isStreaming, stream, cancel, clearPending } =
    useConversationStream()

  // When a different conversation is selected, drop any pending bubbles.
  useEffect(() => {
    clearPending()
  }, [convId, clearPending])

  const historyMsgs = useMemo<MessageViewModel[]>(() => {
    if (!detail?.messages) return []
    return detail.messages.map(toViewModel)
  }, [detail])

  // Auto-scroll on any new content.
  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [historyMsgs, pendingMsgs])

  const handleSend = async (content: string) => {
    if (!convId) return
    await stream(convId, content)
    // Persisted history is now the source of truth — refetch and drop pending.
    await queryClient.invalidateQueries({ queryKey: ["conversation", convId] })
    queryClient.invalidateQueries({ queryKey: ["conversations"] })
    clearPending()
  }

  if (!convId) return <EmptyState />

  return (
    <div className="flex h-full flex-col">
      <header className="flex h-14 items-center gap-2 border-b px-4">
        <MessageSquare className="h-4 w-4 text-muted-foreground" />
        <span className="truncate font-medium">
          {detail?.title || `Conversation #${convId}`}
        </span>
        {detail?.model && (
          <span className="text-xs text-muted-foreground">· {detail.model}</span>
        )}
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
    </div>
  )
}

// --- helpers ---

function toViewModel(m: Message): MessageViewModel {
  const toolCalls =
    m.tool_calls?.map((tc, i) => ({
      key: `${m.id}-tc-${i}`,
      call: {
        id: tc.id,
        type: tc.type,
        name: tc.name,
        arguments: tc.arguments,
      },
    })) ?? undefined

  return {
    id: `db-${m.id}`,
    role: m.role,
    content: m.content ?? "",
    toolCalls,
  }
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
