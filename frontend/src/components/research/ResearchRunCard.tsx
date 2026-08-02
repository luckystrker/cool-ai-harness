import { BookOpen, CheckCircle2, Clock, Loader2, XCircle } from "lucide-react"
import type { ResearchRun } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { cn } from "@/lib/utils"

const STATUS_BADGE: Record<ResearchRun["status"], { label: string; variant: "default" | "secondary" | "success" | "destructive" | "warning"; icon: React.ReactNode }> = {
  queued: { label: "queued", variant: "secondary", icon: <Clock className="h-3 w-3" /> },
  running: { label: "running", variant: "default", icon: <Loader2 className="h-3 w-3 animate-spin" /> },
  completed: { label: "completed", variant: "success", icon: <CheckCircle2 className="h-3 w-3" /> },
  failed: { label: "failed", variant: "destructive", icon: <XCircle className="h-3 w-3" /> },
  cancelled: { label: "cancelled", variant: "secondary", icon: <XCircle className="h-3 w-3" /> },
}

export function ResearchRunCard({
  run,
  selected,
  onClick,
}: {
  run: ResearchRun
  selected: boolean
  onClick: () => void
}) {
  const status = STATUS_BADGE[run.status]
  return (
    <button
      onClick={onClick}
      className={cn(
        "w-full rounded-md border px-3 py-2 text-left transition-colors hover:bg-muted",
        selected && "border-primary/50 bg-muted"
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="min-w-0 flex-1 truncate text-sm font-medium">{run.topic}</span>
        <Badge variant={status.variant} className="shrink-0 gap-1 text-[10px]">
          {status.icon}
          {status.label}
        </Badge>
      </div>
      <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-muted-foreground">
        <span className="flex items-center gap-1">
          <BookOpen className="h-3 w-3" />
          {run.sources_count} sources · {run.citations_count} citations
        </span>
        {run.model && <span>{run.model}</span>}
        <span>{new Date(run.created_at).toLocaleString()}</span>
      </div>
    </button>
  )
}
