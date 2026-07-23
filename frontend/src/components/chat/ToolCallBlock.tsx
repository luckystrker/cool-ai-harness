import { useState } from "react"
import { ChevronRight, Wrench, CheckCircle2, AlertCircle, Clock } from "lucide-react"
import type { ToolCall } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { cn, formatDuration } from "@/lib/utils"

export interface ToolCallBlockProps {
  call: ToolCall
  /** Result payload if the tool has finished. */
  result?: {
    output?: string
    is_error?: boolean
    error?: string | null
    metadata?: Record<string, unknown>
  }
  /** Pending = started but no result yet. */
  pending?: boolean
  /** Waiting for the user to approve/deny the call (permission = "ask"). */
  awaitingApproval?: boolean
}

/** Collapsible block showing a single tool invocation + its result. */
export function ToolCallBlock({ call, result, pending, awaitingApproval }: ToolCallBlockProps) {
  const [open, setOpen] = useState(awaitingApproval)

  const errored = result?.is_error === true
  const durationMs = result?.metadata?.duration_ms
  const duration =
    typeof durationMs === "number" ? formatDuration(durationMs) : undefined
  const statusIcon = awaitingApproval ? (
    <Clock className="h-3.5 w-3.5 animate-pulse text-amber-500" />
  ) : pending ? (
    <Wrench className="h-3.5 w-3.5 animate-pulse text-muted-foreground" />
  ) : errored ? (
    <AlertCircle className="h-3.5 w-3.5 text-destructive" />
  ) : (
    <CheckCircle2 className="h-3.5 w-3.5 text-emerald-500" />
  )
  const badge = awaitingApproval
    ? "waiting"
    : pending
    ? "running"
    : errored
    ? "error"
    : "ok"

  return (
    <div className="rounded-md border bg-muted/40 text-xs">
      <button
        className="flex w-full items-center gap-2 px-3 py-2 text-left"
        onClick={() => setOpen((o) => !o)}
      >
        <ChevronRight
          className={cn("h-3.5 w-3.5 transition-transform", open && "rotate-90")}
        />
        {statusIcon}
        <span className="font-mono font-medium">{call.name}</span>
        <Badge
          variant={
            awaitingApproval
              ? "secondary"
              : errored
              ? "destructive"
              : "secondary"
          }
          className={cn(
            "ml-1",
            awaitingApproval && "border-amber-500/50 text-amber-600"
          )}
        >
          {badge}
        </Badge>
        {duration && (
          <span className="ml-auto font-mono text-[11px] text-muted-foreground/70">
            {duration}
          </span>
        )}
      </button>

      {open && (
        <div className="space-y-2 border-t px-3 py-2">
          {Object.keys(call.arguments ?? {}).length > 0 && (
            <div>
              <div className="mb-1 text-muted-foreground">Arguments</div>
              <pre className="overflow-x-auto rounded bg-background p-2 font-mono text-[11px]">
                {JSON.stringify(call.arguments ?? {}, null, 2)}
              </pre>
            </div>
          )}
          {result && (
            <div>
              <div className="mb-1 text-muted-foreground">
                {errored ? "Error" : "Result"}
              </div>
              <pre
                className={cn(
                  "max-h-64 overflow-auto rounded bg-background p-2 font-mono text-[11px]",
                  errored && "text-destructive"
                )}
              >
                {result.output ?? result.error ?? "(empty)"}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  )
}
