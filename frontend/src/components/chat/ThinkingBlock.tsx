import { useState, useEffect, useRef } from "react"
import { ChevronRight, ChevronDown, Brain } from "lucide-react"
import { cn, formatDuration } from "@/lib/utils"
import { Markdown } from "./Markdown"

export interface ThinkingBlockProps {
  /** Reasoning / chain-of-thought text. */
  content: string
  /** Elapsed time for the whole run, if known. Shown as a muted hint. */
  durationMs?: number
  /** True while the assistant is still streaming. Auto-expands while live. */
  streaming?: boolean
}

/**
 * Collapsible reasoning trace with animated thinking indicator.
 * Auto-expanded while streaming, collapsed by default once the assistant
 * turn is done — reasoning is supporting context, not the headline answer.
 */
export function ThinkingBlock({ content, durationMs, streaming }: ThinkingBlockProps) {
  // Default to open while live so the user sees the model think in real time;
  // collapse once streaming ends.
  const [open, setOpen] = useState(!!streaming)
  const [charCount, setCharCount] = useState(0)
  const prevStreaming = useRef(streaming)

  // Auto-collapse when streaming ends.
  useEffect(() => {
    if (prevStreaming.current && !streaming) {
      setOpen(false)
    }
    prevStreaming.current = streaming
  }, [streaming])

  // Animate character count while streaming.
  useEffect(() => {
    if (streaming) {
      setCharCount(content.length)
    }
  }, [content, streaming])

  return (
    <div className={cn(
      "rounded-lg border text-xs transition-colors",
      streaming
        ? "border-violet-500/30 bg-violet-500/5"
        : "border-dashed border-muted-foreground/30 bg-muted/30"
    )}>
      <button
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-muted-foreground"
        onClick={() => setOpen((o) => !o)}
      >
        {open ? (
          <ChevronDown className="h-3.5 w-3.5 transition-transform" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 transition-transform" />
        )}
        <Brain className={cn(
          "h-3.5 w-3.5",
          streaming ? "animate-pulse text-violet-500" : "text-muted-foreground/70"
        )} />
        <span className="font-medium">
          {streaming ? "Thinking…" : "Thought process"}
        </span>
        {streaming && charCount > 0 && (
          <span className="font-mono text-[10px] text-violet-400/70">
            {charCount} chars
          </span>
        )}
        {durationMs != null && !streaming && (
          <span className="ml-auto font-mono text-[11px] text-muted-foreground/80">
            {formatDuration(durationMs)}
          </span>
        )}
      </button>

      {open && content && (
        <div className={cn(
          "border-t px-3 py-2 text-muted-foreground",
          streaming ? "border-violet-500/20" : "border-muted-foreground/20"
        )}>
          <div className={cn("max-h-60 overflow-y-auto", streaming && "animate-in fade-in")}>
            <Markdown content={content} />
          </div>
          {streaming && (
            <div className="mt-1 flex items-center gap-1 text-violet-400/60">
              <span className="inline-block h-2 w-2 animate-ping rounded-full bg-violet-400/50" />
              <span className="text-[10px] italic">reasoning…</span>
            </div>
          )}
        </div>
      )}
    </div>
  )
}
