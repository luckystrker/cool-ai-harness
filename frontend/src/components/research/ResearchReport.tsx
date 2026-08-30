import { AlertTriangle, Camera, Download, ExternalLink, FileText } from "lucide-react"
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
            <a href={deepResearchApi.exportUrl(run.id, "pdf")}>
              <Download className="h-3.5 w-3.5" /> PDF
            </a>
          </Button>
          <Button asChild variant="outline" size="sm">
            <a href={deepResearchApi.exportUrl(run.id, "docx")}>
              <Download className="h-3.5 w-3.5" /> DOCX
            </a>
          </Button>
          <Button asChild variant="outline" size="sm">
            <a href={deepResearchApi.exportUrl(run.id, "html")}>
              <Download className="h-3.5 w-3.5" /> HTML
            </a>
          </Button>
        </div>
      </div>

      {run.browser_activity.length > 0 && (
        <details className="rounded-md border bg-muted/20">
          <summary className="cursor-pointer px-3 py-2 text-sm font-medium">
            Browser activity ({run.browser_activity.length} actions)
          </summary>
          <ol className="max-h-72 space-y-2 overflow-y-auto border-t p-3">
            {run.browser_activity.map((action, index) => (
              <li key={`${action.id}-${index}`} className="rounded bg-background p-2 text-xs">
                <div className="flex items-center gap-2">
                  <span className="font-mono font-medium">{action.name}</span>
                  <Badge variant={action.status === "error" ? "destructive" : "secondary"}>
                    {action.status}
                  </Badge>
                  <span className="ml-auto text-muted-foreground">
                    {new Date(action.created_at).toLocaleTimeString()}
                  </span>
                </div>
                <pre className="mt-1 overflow-x-auto text-[11px] text-muted-foreground">
                  {JSON.stringify(action.arguments)}
                </pre>
                {typeof action.metadata?.screenshot_url === "string" && (
                  <a className="mt-2 inline-flex items-center gap-1 text-blue-600 underline" href={action.metadata.screenshot_url} target="_blank" rel="noreferrer">
                    <Camera className="h-3 w-3" /> View screenshot
                  </a>
                )}
              </li>
            ))}
          </ol>
        </details>
      )}

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
