import { User, Sparkles, Terminal } from "lucide-react"
import { cn, formatDuration, formatMessageTime } from "@/lib/utils"
import type { Plan, UsagePayload } from "@/api/types"
import { Markdown } from "./Markdown"
import { ToolCallBlock, type ToolCallBlockProps } from "./ToolCallBlock"
import { ThinkingBlock } from "./ThinkingBlock"
import { ApprovalCard, type InlineApproval } from "./ApprovalCard"
import { PlanCard } from "./PlanCard"

/**
 * One visual block in an assistant turn's interleaved stream. While the agent
 * runs it alternates "think → call tool → think → call tool …"; preserving
 * that order lets the live bubble render the blocks interleaved instead of
 * merging every thought into a single block at the top.
 */
export type AssistantStreamBlock =
  | { type: "thinking"; text: string }
  | { type: "tools"; calls: (ToolCallBlockProps & { key: string })[] }

export interface MessageViewModel {
  id: string
  role: "user" | "assistant" | "system" | "tool"
  content: string
  /** Tool calls attached to this assistant message, with optional result. */
  toolCalls?: (ToolCallBlockProps & { key: string })[]
  /** Reasoning / chain-of-thought text, when the provider exposes one. */
  thinking?: string
  /** Ordered thinking/tool blocks for interleaved live-stream rendering. */
  blocks?: AssistantStreamBlock[]
  /** Total elapsed time for the assistant turn (live or from history). */
  elapsedMs?: number
  /** Token usage from the terminal finish event, when reported. */
  usage?: UsagePayload
  /** Finish reason (e.g. "stop", "max_iterations"). */
  finishReason?: string
  /** Streaming = assistant currently generating; show caret. */
  streaming?: boolean
  /** Inline approval request rendered in the chat flow (approve/deny). */
  approval?: InlineApproval
  /** Which model produced this assistant message (history). */
  model?: string | null
  /** When the message was sent (ISO string). */
  createdAt?: string | null
  /** Plan generated during this turn (Фаза 2 §1 Planning Mode). */
  plan?: Plan
}

const ROLE_META = {
  user: { label: "You", icon: User, color: "bg-primary text-primary-foreground" },
  assistant: { label: "Assistant", icon: Sparkles, color: "bg-violet-500 text-white" },
  system: { label: "System", icon: Terminal, color: "bg-muted-foreground text-background" },
  tool: { label: "Tool", icon: Terminal, color: "bg-muted-foreground text-background" },
} as const

export function MessageBubble({
  msg,
  onRespondApproval,
  onPlanApprove,
  onPlanExecute,
}: {
  msg: MessageViewModel
  /** Callback to resolve an inline approval (approve/deny). */
  onRespondApproval?: (approved: boolean) => void
  /** Callback to approve/reject a plan (Фаза 2 §1). */
  onPlanApprove?: (approved: boolean) => void
  /** Callback to execute an approved plan. */
  onPlanExecute?: () => void
}) {
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
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          <span className="font-medium">{meta.label}</span>
          {isAssistant && msg.model && (
            <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[10px]">
              {msg.model}
            </span>
          )}
          {msg.createdAt && (
            <span className="text-[10px] text-muted-foreground/70">
              {formatMessageTime(msg.createdAt)}
            </span>
          )}
        </div>

        {/* Reasoning + tool blocks. While streaming, `blocks` preserves the
            real interleaved order (think → tool → think → tool). Persisted
            history falls back to the flat thinking-then-tools layout because
            each loop iteration is already its own message row. */}
        {isAssistant && msg.blocks && msg.blocks.length > 0 ? (
          <InterleavedBlocks
            blocks={msg.blocks}
            streaming={msg.streaming}
            elapsedMs={msg.elapsedMs}
          />
        ) : (
          <>
            {/* Reasoning trace sits above the answer, collapsed by default. */}
            {isAssistant && msg.thinking && (
              <ThinkingBlock
                content={msg.thinking}
                durationMs={msg.elapsedMs}
                streaming={msg.streaming}
              />
            )}

            {/* Tool execution blocks sit between reasoning and the final response. */}
            {msg.toolCalls && msg.toolCalls.length > 0 && (
              <div className="flex w-full flex-col gap-1.5">
                {msg.toolCalls.map(({ key, ...blockProps }) => (
                  <ToolCallBlock key={key} {...blockProps} />
                ))}
              </div>
            )}
          </>
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

        {/* Inline approval card — approve/deny directly in the chat flow. */}
        {msg.approval && (
          <ApprovalCard
            approval={msg.approval}
            onRespond={onRespondApproval ?? (() => {})}
          />
        )}

        {/* Plan card — shows generated plan with approve/execute actions (Фаза 2 §1). */}
        {isAssistant && msg.plan && (
          <PlanCard
            plan={msg.plan}
            onApprove={onPlanApprove}
            onExecute={onPlanExecute}
          />
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
                <span>{finishReasonLabel(msg.finishReason)}</span>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

/**
 * Render the ordered thinking/tool blocks of a live assistant turn so they
 * alternate exactly as the agent produced them (think → tool → think → tool).
 * Only the most recent thinking block is treated as "live" (auto-expanded);
 * earlier ones collapse so the focus stays on the current step.
 */
function InterleavedBlocks({
  blocks,
  streaming,
  elapsedMs,
}: {
  blocks: AssistantStreamBlock[]
  streaming?: boolean
  elapsedMs?: number
}) {
  const lastThinkingIdx = blocks.map((b) => b.type).lastIndexOf("thinking")
  return (
    <>
      {blocks.map((block, i) =>
        block.type === "thinking" ? (
          <ThinkingBlock
            key={`thinking-${i}`}
            content={block.text}
            streaming={streaming && i === lastThinkingIdx}
            durationMs={i === lastThinkingIdx ? elapsedMs : undefined}
          />
        ) : (
          <div key={`tools-${i}`} className="flex w-full flex-col gap-1.5">
            {block.calls.map(({ key, ...blockProps }) => (
              <ToolCallBlock key={key} {...blockProps} />
            ))}
          </div>
        )
      )}
    </>
  )
}

/** Human-friendly labels for non-"stop" finish reasons shown in the footnote. */
function finishReasonLabel(reason: string): string {
  switch (reason) {
    case "max_iterations":
      return "tool limit reached"
    case "token_limit":
      return "token limit reached"
    case "cost_limit":
      return "cost limit reached"
    case "budget_exceeded":
      return "budget exceeded"
    case "cancelled":
      return "cancelled"
    case "error":
      return "error"
    default:
      return reason
  }
}
