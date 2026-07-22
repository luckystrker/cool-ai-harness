import { useState } from "react"
import { ChevronRight, Brain } from "lucide-react"
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
 * Collapsible reasoning trace. Auto-expanded while streaming, collapsed by
 * default once the assistant turn is done — reasoning is supporting context,
 * not the headline answer.
 */
export function ThinkingBlock({ content, durationMs, streaming }: ThinkingBlockProps) {
  // Default to open while live so the user sees the model think in real time;
  // collapse once streaming ends.
  const [open, setOpen] = useState(!!streaming)

  return (
    <div className="rounded-lg border border-dashed border-muted-foreground/30 bg-muted/30 text-xs">
      <button
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-muted-foreground"
        onClick={() => setOpen((o) => !o)}
      >
        <ChevronRight
          className={cn("h-3.5 w-3.5 transition-transform", open && "rotate-90")}
        />
        <Brain className={cn("h-3.5 w-3.5", streaming && "animate-pulse text-violet-500")} />
        <span className="font-medium">
          {streaming ? "Thinking…" : "Thought process"}
        </span>
        {durationMs != null && (
          <span className="font-mono text-[11px] text-muted-foreground/80">
            {formatDuration(durationMs)}
          </span>
        )}
      </button>

      {open && content && (
        <div className="border-t border-muted-foreground/20 px-3 py-2 text-muted-foreground">
          <Markdown content={content} />
        </div>
      )}
    </div>
  )
}
