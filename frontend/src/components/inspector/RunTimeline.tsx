/** RunTimeline: vertical stepper showing per-iteration breakdown. */

import type { IterationDetail } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { cn } from "@/lib/utils"

interface RunTimelineProps {
  iterations: IterationDetail[]
  totalDurationMs: number | null
}

function formatMs(ms: number | null): string {
  if (ms === null) return "—"
  if (ms < 1000) return `${ms}ms`
  return `${(ms / 1000).toFixed(2)}s`
}

function formatTokens(usage: Record<string, unknown> | null): string {
  if (!usage) return "—"
  const total = usage.total_tokens as number | undefined
  return total != null ? `${total} tok` : "—"
}

export function RunTimeline({ iterations, totalDurationMs }: RunTimelineProps) {
  if (iterations.length === 0) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        No iteration data available for this run.
      </p>
    )
  }

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center justify-between text-sm">
          <span>Timeline ({iterations.length} iteration{iterations.length !== 1 ? "s" : ""})</span>
          <Badge variant="secondary">{formatMs(totalDurationMs)} total</Badge>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className="relative space-y-0">
          {iterations.map((iter, idx) => (
            <div key={iter.iteration} className="relative flex gap-3 pb-4 last:pb-0">
              {/* Vertical connector line */}
              {idx < iterations.length - 1 && (
                <div className="absolute left-[11px] top-6 h-full w-px bg-border" />
              )}
              {/* Step dot */}
              <div
                className={cn(
                  "z-10 flex h-6 w-6 shrink-0 items-center justify-center rounded-full border text-xs font-medium",
                  iter.finish_reason
                    ? "border-green-500 bg-green-500/10 text-green-700"
                    : "border-primary bg-primary/10 text-primary"
                )}
              >
                {iter.iteration}
              </div>
              {/* Content */}
              <div className="min-w-0 flex-1 space-y-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">Iteration {iter.iteration}</span>
                  <Badge variant="outline" className="text-xs">
                    {formatMs(iter.duration_ms)}
                  </Badge>
                  <Badge variant="outline" className="text-xs">
                    {formatTokens(iter.usage)}
                  </Badge>
                  {iter.model && (
                    <Badge variant="secondary" className="text-xs">
                      {iter.model}
                    </Badge>
                  )}
                  {iter.finish_reason && (
                    <Badge className="bg-green-600 text-xs text-white">
                      {iter.finish_reason}
                    </Badge>
                  )}
                </div>
                {/* Tool calls */}
                {iter.tool_calls.length > 0 && (
                  <div className="space-y-1 pt-1">
                    {iter.tool_calls.map((tc, i) => (
                      <div
                        key={i}
                        className="rounded-md border bg-muted/50 px-2 py-1 text-xs font-mono"
                      >
                        <span className="text-primary">{String(tc.name ?? "unknown")}</span>
                        {tc.arguments != null && Object.keys(tc.arguments as object).length > 0 ? (
                          <span className="ml-1 text-muted-foreground">
                            {JSON.stringify(tc.arguments).slice(0, 120)}
                          </span>
                        ) : null}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}
