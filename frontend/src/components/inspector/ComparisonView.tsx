/** ComparisonView: side-by-side comparison of two runs. */

import type { RunComparison } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { RunTimeline } from "./RunTimeline"

interface ComparisonViewProps {
  comparison: RunComparison
}

function DeltaBadge({ value, unit }: { value: number | null; unit: string }) {
  if (value === null) return <Badge variant="outline">—</Badge>
  const positive = value > 0
  const negative = value < 0
  return (
    <Badge
      variant="outline"
      className={
        positive
          ? "border-orange-300 bg-orange-50 text-orange-700"
          : negative
            ? "border-green-300 bg-green-50 text-green-700"
            : ""
      }
    >
      {positive ? "+" : ""}
      {value}
      {unit}
    </Badge>
  )
}

export function ComparisonView({ comparison }: ComparisonViewProps) {
  const { run_a, run_b } = comparison

  return (
    <div className="space-y-4">
      {/* Metric deltas */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-sm">Metric Deltas (B − A)</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <div className="space-y-1">
              <p className="text-xs text-muted-foreground">Tokens</p>
              <DeltaBadge value={comparison.delta_tokens} unit=" tok" />
            </div>
            <div className="space-y-1">
              <p className="text-xs text-muted-foreground">Cost</p>
              <DeltaBadge
                value={
                  comparison.delta_cost_usd !== null
                    ? Math.round(comparison.delta_cost_usd * 10000) / 10000
                    : null
                }
                unit="$"
              />
            </div>
            <div className="space-y-1">
              <p className="text-xs text-muted-foreground">Iterations</p>
              <DeltaBadge value={comparison.delta_iterations} unit="" />
            </div>
            <div className="space-y-1">
              <p className="text-xs text-muted-foreground">Duration</p>
              <DeltaBadge value={comparison.delta_duration_ms} unit="ms" />
            </div>
          </div>
          <div className="mt-4 grid grid-cols-2 gap-4 text-xs text-muted-foreground">
            <div>
              <span className="font-medium text-foreground">Run A (#{run_a.id})</span>:{" "}
              {run_a.model ?? "—"}, {run_a.iterations} iter, {run_a.status}
            </div>
            <div>
              <span className="font-medium text-foreground">Run B (#{run_b.id})</span>:{" "}
              {run_b.model ?? "—"}, {run_b.iterations} iter, {run_b.status}
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Side-by-side timelines */}
      <div className="grid gap-4 lg:grid-cols-2">
        <div>
          <h4 className="mb-2 text-xs font-medium text-muted-foreground">
            Run A (#{run_a.id})
          </h4>
          <RunTimeline iterations={comparison.iterations_a} totalDurationMs={null} />
        </div>
        <div>
          <h4 className="mb-2 text-xs font-medium text-muted-foreground">
            Run B (#{run_b.id})
          </h4>
          <RunTimeline iterations={comparison.iterations_b} totalDurationMs={null} />
        </div>
      </div>
    </div>
  )
}
