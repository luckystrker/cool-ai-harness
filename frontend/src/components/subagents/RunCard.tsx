import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Loader2, Square, Trash2, CheckCircle2, XCircle, Clock } from "lucide-react"
import { toast } from "sonner"
import { subagentsApi } from "@/api/subagents"
import type { SubagentRun } from "@/api/types"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

const STATUS_META: Record<string, { icon: typeof Clock; color: string; label: string }> = {
  queued: { icon: Clock, color: "text-muted-foreground", label: "Queued" },
  running: { icon: Loader2, color: "text-blue-500", label: "Running" },
  completed: { icon: CheckCircle2, color: "text-green-500", label: "Completed" },
  failed: { icon: XCircle, color: "text-red-500", label: "Failed" },
  cancelled: { icon: Square, color: "text-yellow-500", label: "Cancelled" },
}

export function RunCard({ run }: { run: SubagentRun }) {
  const queryClient = useQueryClient()
  const meta = STATUS_META[run.status] ?? STATUS_META.queued
  const Icon = meta.icon
  const isTerminal = ["completed", "failed", "cancelled"].includes(run.status)

  const cancelMutation = useMutation({
    mutationFn: () => subagentsApi.cancelRun(run.id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["subagent-runs"] })
      toast.success("Subagent cancelled")
    },
  })

  const deleteMutation = useMutation({
    mutationFn: () => subagentsApi.deleteRun(run.id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["subagent-runs"] })
    },
  })

  return (
    <div className="rounded-lg border p-3">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <Icon
              className={cn(
                "h-4 w-4 shrink-0",
                meta.color,
                run.status === "running" && "animate-spin"
              )}
            />
            <span className="truncate text-sm font-medium">
              {run.name || `Run #${run.id}`}
            </span>
            <span className={cn("text-xs", meta.color)}>{meta.label}</span>
          </div>
          <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">{run.prompt}</p>
          {run.result_summary && (
            <p className="mt-1 line-clamp-3 text-xs text-foreground/80">
              {run.result_summary}
            </p>
          )}
          {run.error && (
            <p className="mt-1 text-xs text-red-500">{run.error}</p>
          )}
          {run.usage && (
            <p className="mt-1 text-[10px] text-muted-foreground/70">
              {(run.usage as Record<string, number>).total_tokens ?? 0} tokens
            </p>
          )}
        </div>

        <div className="flex shrink-0 gap-1">
          {!isTerminal && (
            <Button
              variant="ghost"
              size="icon"
              className="h-9 w-9 md:h-6 md:w-6"
              onClick={() => cancelMutation.mutate()}
              title="Cancel"
            >
              <Square className="h-3.5 w-3.5 md:h-3 md:w-3" />
            </Button>
          )}
          {isTerminal && (
            <Button
              variant="ghost"
              size="icon"
              className="h-9 w-9 md:h-6 md:w-6"
              onClick={() => deleteMutation.mutate()}
              title="Delete"
            >
              <Trash2 className="h-3.5 w-3.5 md:h-3 md:w-3" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}
