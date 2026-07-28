import { useQuery } from "@tanstack/react-query"
import { Loader2 } from "lucide-react"
import { memoryApi } from "@/api/memory"
import { Badge } from "@/components/ui/badge"

/** Expandable "why is this remembered" breakdown for a memory card. */
export function ExplainPanel({ memoryId }: { memoryId: number }) {
  const { data, isLoading, error } = useQuery({
    queryKey: ["memory-explain", memoryId],
    queryFn: () => memoryApi.explain(memoryId),
  })

  if (isLoading) {
    return (
      <div className="flex items-center gap-2 py-2 text-xs text-muted-foreground">
        <Loader2 className="h-3 w-3 animate-spin" /> Loading explanation…
      </div>
    )
  }
  if (error || !data) {
    return (
      <div className="py-2 text-xs text-muted-foreground">
        Could not load explanation.
      </div>
    )
  }

  const s = data.score
  const SOURCE_LABELS: Record<string, string> = {
    user_explicit: "You added this",
    agent: "Agent stored this",
    agent_extraction: "Extracted from a conversation",
    system: "System-generated",
  }

  return (
    <div className="space-y-2 py-2 text-xs text-muted-foreground">
      <div className="flex flex-wrap gap-2">
        <Badge variant="outline">{SOURCE_LABELS[data.source] ?? data.source}</Badge>
        <Badge variant="outline">scope: {data.scope}</Badge>
        <Badge variant="outline">accessed {data.access_count}x</Badge>
        <Badge variant="outline">{data.score.age_days.toFixed(0)} days old</Badge>
      </div>
      <div>
        <span className="font-medium text-foreground">Why it ranks here</span> (total{" "}
        {s.total.toFixed(3)}):
      </div>
      <div className="grid grid-cols-2 gap-x-4 gap-y-1">
        <ScoreRow label="Importance" value={s.importance} />
        <ScoreRow label="Recency" value={s.recency} />
        <ScoreRow label="Confidence" value={s.confidence} />
        <ScoreRow label="Type priority" value={s.type_priority} />
      </div>
      <div className="text-[11px] opacity-80">
        Stored {new Date(data.created_at).toLocaleString()}
        {data.last_accessed_at &&
          ` · last accessed ${new Date(data.last_accessed_at).toLocaleString()}`}
      </div>
    </div>
  )
}

function ScoreRow({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-24">{label}</span>
      <div className="h-1.5 flex-1 overflow-hidden rounded bg-muted">
        <div
          className="h-full bg-primary/60"
          style={{ width: `${Math.min(100, value * 100)}%` }}
        />
      </div>
      <span className="w-10 text-right tabular-nums">{value.toFixed(3)}</span>
    </div>
  )
}
