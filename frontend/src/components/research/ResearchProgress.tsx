import { CheckCircle2, CircleDashed, Globe, Loader2, XCircle } from "lucide-react"
import type { ResearchProgressState } from "@/hooks/useResearchStream"
import { cn } from "@/lib/utils"

const STAGE_LABELS: Record<NonNullable<ResearchProgressState["stage"]>, string> = {
  decompose: "Decomposing topic into sub-questions",
  gather: "Researching sub-questions in parallel",
  synthesize: "Synthesizing the cited report",
}

const SUBSTATUS_ICON = {
  pending: <CircleDashed className="h-3.5 w-3.5 text-muted-foreground" />,
  running: <Loader2 className="h-3.5 w-3.5 animate-spin text-blue-500" />,
  completed: <CheckCircle2 className="h-3.5 w-3.5 text-emerald-500" />,
  failed: <XCircle className="h-3.5 w-3.5 text-red-500" />,
  empty: <CircleDashed className="h-3.5 w-3.5 text-muted-foreground" />,
} as const

/** Live progress panel for an in-flight research run (SSE-fed). */
export function ResearchProgress({ progress }: { progress: ResearchProgressState }) {
  const running = !progress.terminal
  return (
    <div className="space-y-4">
      {running && (
        <div className="space-y-1.5">
          {(["decompose", "gather", "synthesize"] as const).map((stage) => {
            const order = { decompose: 0, gather: 1, synthesize: 2 }
            const active = progress.stage === stage
            const passed =
              (progress.stage && order[progress.stage] > order[stage]) ||
              progress.stage === "synthesize"
            return (
              <div
                key={stage}
                className={cn(
                  "flex items-center gap-2 rounded-md border px-3 py-2 text-sm",
                  active
                    ? "border-blue-300/50 bg-blue-500/10"
                    : passed
                      ? "border-border text-foreground"
                      : "border-border text-muted-foreground"
                )}
              >
                {active ? (
                  <Loader2 className="h-4 w-4 animate-spin text-blue-500" />
                ) : passed ? (
                  <CheckCircle2 className="h-4 w-4 text-emerald-500" />
                ) : (
                  <CircleDashed className="h-4 w-4" />
                )}
                {STAGE_LABELS[stage]}
              </div>
            )
          })}
        </div>
      )}

      {progress.subQuestions.length > 0 && (
        <div className="space-y-1">
          <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            Sub-questions
          </span>
          <ul className="space-y-1">
            {progress.subQuestions.map((q) => (
              <li key={q.index} className="flex items-start gap-2 text-sm">
                <span className="mt-0.5">{SUBSTATUS_ICON[q.status]}</span>
                <span
                  className={cn(
                    q.status === "completed"
                      ? "text-foreground"
                      : q.status === "failed"
                        ? "text-red-500"
                        : "text-muted-foreground"
                  )}
                >
                  {q.text || `Sub-question ${q.index + 1}`}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {progress.sources.length > 0 && (
        <div className="space-y-1">
          <span className="flex items-center gap-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            <Globe className="h-3 w-3" /> Sources found ({progress.sources.length})
          </span>
          <ul className="space-y-1">
            {progress.sources.slice(-8).map((s) => (
              <li key={s.url} className="flex items-center gap-1.5 truncate text-xs">
                <span className="h-1 w-1 shrink-0 rounded-full bg-emerald-500" />
                <span className="truncate text-muted-foreground">{s.title || s.url}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  )
}
