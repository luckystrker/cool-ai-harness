import type { ResearchRunDetail } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { ResearchReport } from "@/components/research/ResearchReport"

/**
 * Side-by-side comparison of two research runs (e.g. different models).
 */
export function ResearchCompare({
  runs,
}: {
  runs: [ResearchRunDetail, ResearchRunDetail]
}) {
  return (
    <div className="grid flex-1 gap-4 lg:grid-cols-2">
      {runs.map((run) => (
        <div
          key={run.id}
          className="flex min-w-0 flex-col gap-2 overflow-y-auto rounded-md border p-4"
        >
          <div className="flex items-center justify-between gap-2">
            <span className="truncate text-sm font-semibold">{run.model ?? "default model"}</span>
            <Badge variant="secondary" className="text-[10px]">
              run #{run.id}
            </Badge>
          </div>
          <div className="min-w-0 flex-1">
            <ResearchReport run={run} />
          </div>
        </div>
      ))}
    </div>
  )
}
