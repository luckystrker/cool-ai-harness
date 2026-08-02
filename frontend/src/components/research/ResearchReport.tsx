import { AlertTriangle, Download, ExternalLink, FileText } from "lucide-react"
import { deepResearchApi } from "@/api/research"
import type { ResearchRunDetail } from "@/api/types"
import { Markdown } from "@/components/chat/Markdown"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

/**
 * Renders a completed research report with clickable [n] citations.
 *
 * Citation markers in the markdown are rewritten to anchors pointing at the
 * numbered bibliography below; each bibliography entry links back with its
 * source URL, confidence and conflict markers.
 */
export function ResearchReport({ run }: { run: ResearchRunDetail }) {
  if (!run.report_markdown) {
    return (
      <p className="text-sm text-muted-foreground">
        No report available for this run{run.error ? ` (${run.error})` : ""}.
      </p>
    )
  }

  // Rewrite [n] markers into anchors: [1] → [1](#source-1).
  const markdown = run.report_markdown.replace(/\[(\d{1,3})\]/g, (_, n) => `[${n}](#source-${n})`)

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-2">
        <h2 className="min-w-0 truncate text-lg font-semibold">{run.topic}</h2>
        <div className="flex shrink-0 gap-1.5">
          <Button asChild variant="outline" size="sm">
            <a href={deepResearchApi.exportUrl(run.id, "md")}>
              <FileText className="h-3.5 w-3.5" /> MD
            </a>
          </Button>
          <Button asChild variant="outline" size="sm">
            <a href={deepResearchApi.exportUrl(run.id, "html")}>
              <Download className="h-3.5 w-3.5" /> HTML
            </a>
          </Button>
        </div>
      </div>

      <Markdown content={markdown} />

      <div className="space-y-1 border-t pt-3">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
          Bibliography ({run.sources.length} sources)
        </span>
        <ol className="space-y-2">
          {run.sources.map((source, idx) => (
            <li key={idx} id={`source-${idx + 1}`} className="scroll-mt-4 text-sm">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="text-xs text-muted-foreground">[{idx + 1}]</span>
                <a
                  href={source.url}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 truncate text-blue-600 underline dark:text-blue-400"
                >
                  <span className="truncate">{source.title || source.url}</span>
                  <ExternalLink className="h-3 w-3 shrink-0" />
                </a>
                <ConfidenceBadge confidence={source.confidence} />
                {source.conflict && (
                  <Badge variant="warning" className="gap-1">
                    <AlertTriangle className="h-3 w-3" /> conflict
                  </Badge>
                )}
              </div>
              {source.snippet && (
                <p className="mt-0.5 text-xs text-muted-foreground">{source.snippet}</p>
              )}
            </li>
          ))}
        </ol>
      </div>
    </div>
  )
}

export function ConfidenceBadge({
  confidence,
  className,
}: {
  confidence: string
  className?: string
}) {
  const variant =
    confidence === "high" ? "success" : confidence === "low" ? "destructive" : "warning"
  return (
    <Badge variant={variant} className={cn("normal-case", className)}>
      {confidence}
    </Badge>
  )
}
