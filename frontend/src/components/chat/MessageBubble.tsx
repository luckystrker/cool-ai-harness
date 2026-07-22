import { User, Sparkles, Terminal } from "lucide-react"
import { cn, formatDuration } from "@/lib/utils"
import type { UsagePayload } from "@/api/types"
import { Markdown } from "./Markdown"
import { ToolCallBlock, type ToolCallBlockProps } from "./ToolCallBlock"
import { ThinkingBlock } from "./ThinkingBlock"

export interface MessageViewModel {
  id: string
  role: "user" | "assistant" | "system" | "tool"
  content: string
  /** Tool calls attached to this assistant message, with optional result. */
  toolCalls?: (ToolCallBlockProps & { key: string })[]
  /** Reasoning / chain-of-thought text, when the provider exposes one. */
  thinking?: string
  /** Total elapsed time for the assistant turn, when known (live stream). */
  elapsedMs?: number
  /** Token usage from the terminal finish event, when reported. */
  usage?: UsagePayload
  /** Finish reason (e.g. "stop", "max_iterations"). */
  finishReason?: string
  /** Streaming = assistant currently generating; show caret. */
  streaming?: boolean
}

const ROLE_META = {
  user: { label: "You", icon: User, color: "bg-primary text-primary-foreground" },
  assistant: { label: "Assistant", icon: Sparkles, color: "bg-violet-500 text-white" },
  system: { label: "System", icon: Terminal, color: "bg-muted-foreground text-background" },
  tool: { label: "Tool", icon: Terminal, color: "bg-muted-foreground text-background" },
} as const

export function MessageBubble({ msg }: { msg: MessageViewModel }) {
  if (msg.role === "tool") return null // tool results render inside the assistant message that called them
  const meta = ROLE_META[msg.role]
  const Icon = meta.icon
  const isAssistant = msg.role === "assistant"

  // Footnote: elapsed time / token usage, shown once the assistant turn is done.
  const totalTokens = msg.usage?.total_tokens
  const showFootnote =
    isAssistant && !msg.streaming && (msg.elapsedMs != null || totalTokens != null)

  return (
    <div
      className={cn(
        "group flex gap-3 px-4 py-4",
        msg.role === "user" && "flex-row-reverse"
      )}
    >
      <div
        className={cn(
          "flex h-7 w-7 shrink-0 items-center justify-center rounded-md",
          meta.color
        )}
      >
        <Icon className="h-4 w-4" />
      </div>

      <div
        className={cn(
          "flex min-w-0 max-w-[85%] flex-col gap-2",
          msg.role === "user" && "items-end"
        )}
      >
        <div className="text-xs font-medium text-muted-foreground">{meta.label}</div>

        {/* Reasoning trace sits above the answer, collapsed by default. */}
        {isAssistant && msg.thinking && (
          <ThinkingBlock
            content={msg.thinking}
            durationMs={msg.elapsedMs}
            streaming={msg.streaming}
          />
        )}

        {msg.content && (
          <div
            className={cn(
              "rounded-lg px-3 py-2",
              msg.role === "user"
                ? "bg-primary text-primary-foreground"
                : "bg-muted/50"
            )}
          >
            {msg.role === "user" ? (
              <p className="whitespace-pre-wrap text-sm">{msg.content}</p>
            ) : (
              <>
                <Markdown content={msg.content} />
                {msg.streaming && (
                  <span className="ml-0.5 inline-block h-3.5 w-1.5 animate-pulse bg-foreground/70 align-text-bottom" />
                )}
              </>
            )}
          </div>
        )}

        {msg.toolCalls && msg.toolCalls.length > 0 && (
          <div className="flex w-full flex-col gap-1.5">
            {msg.toolCalls.map(({ key, ...blockProps }) => (
              <ToolCallBlock key={key} {...blockProps} />
            ))}
          </div>
        )}

        {/* Assistant currently running tools but no text yet — show a hint. */}
        {isAssistant && msg.streaming && !msg.content && !(msg.toolCalls?.length) && !msg.thinking && (
          <div className="rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
            Thinking…
          </div>
        )}

        {showFootnote && (
          <div className="flex items-center gap-2 px-1 text-[11px] text-muted-foreground/70">
            {msg.elapsedMs != null && <span>{formatDuration(msg.elapsedMs)}</span>}
            {msg.elapsedMs != null && totalTokens != null && <span>·</span>}
            {totalTokens != null && <span>{totalTokens} tokens</span>}
            {msg.finishReason && msg.finishReason !== "stop" && (
              <>
                <span>·</span>
                <span>{msg.finishReason}</span>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
