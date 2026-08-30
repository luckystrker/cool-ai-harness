/** InspectorPage: Debug / Inspector Mode (Фаза 1.5 §6).
 *
 * Provides run timeline inspection, side-by-side comparison, and replay.
 */

import { useState } from "react"
import { useMutation, useQuery } from "@tanstack/react-query"
import { Bug, GitCompareArrows, Loader2, Play } from "lucide-react"
import { toast } from "sonner"
import { compareRuns, getRunTimeline, replayRun } from "@/api/inspector"
import { conversationsApi } from "@/api/conversations"
import type { RunOut, RunTimeline as RunTimelineType } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import { ComparisonView } from "@/components/inspector/ComparisonView"
import { RunTimeline } from "@/components/inspector/RunTimeline"
import { api, getErrorDescription } from "@/api/client"

type Mode = "timeline" | "compare"

export function InspectorPage() {
  const [mode, setMode] = useState<Mode>("timeline")
  const [selectedConvId, setSelectedConvId] = useState<number | null>(null)
  const [selectedRunId, setSelectedRunId] = useState<number | null>(null)
  const [compareRunId, setCompareRunId] = useState<number | null>(null)
  const [replayModel, setReplayModel] = useState("")

  // Load conversations for the selector.
  const { data: conversations = [] } = useQuery({
    queryKey: ["conversations"],
    queryFn: conversationsApi.list,
  })

  // Load runs for the selected conversation.
  const { data: runs = [] } = useQuery({
    queryKey: ["runs", selectedConvId],
    queryFn: () => api.get<RunOut[]>(`/api/conversations/${selectedConvId}/runs`),
    enabled: selectedConvId !== null,
  })

  // Load timeline for the selected run.
  const { data: timeline, isLoading: timelineLoading } = useQuery({
    queryKey: ["timeline", selectedConvId, selectedRunId],
    queryFn: () => getRunTimeline(selectedConvId!, selectedRunId!),
    enabled: selectedConvId !== null && selectedRunId !== null && mode === "timeline",
  })

  // Load comparison.
  const { data: comparison, isLoading: compareLoading } = useQuery({
    queryKey: ["compare", selectedRunId, compareRunId],
    queryFn: () => compareRuns(selectedRunId!, compareRunId!),
    enabled: selectedRunId !== null && compareRunId !== null && mode === "compare",
  })

  // Replay mutation.
  const replayMutation = useMutation({
    mutationFn: () =>
      replayRun(selectedConvId!, selectedRunId!, {
        model: replayModel || undefined,
      }),
    onSuccess: (data) => {
      toast.success(`Replay started: new run #${data.new_run_id}`)
    },
    onError: (error) =>
      toast.error("Run replay did not start", {
        description: getErrorDescription(error, "Select a run and try again."),
      }),
  })

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex h-14 items-center gap-3 border-b px-4">
        <Bug className="h-5 w-5 text-primary" />
        <div>
          <h1 className="font-semibold">Run inspector</h1>
          <p className="hidden text-xs text-muted-foreground sm:block">
            Review an agent run step by step or compare two runs.
          </p>
        </div>
        <div className="ml-auto flex gap-1">
          <Button
            variant={mode === "timeline" ? "default" : "ghost"}
            size="sm"
            onClick={() => setMode("timeline")}
          >
            Timeline
          </Button>
          <Button
            variant={mode === "compare" ? "default" : "ghost"}
            size="sm"
            onClick={() => setMode("compare")}
          >
            <GitCompareArrows className="mr-1 h-3.5 w-3.5" />
            Compare
          </Button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col overflow-hidden md:flex-row">
        {/* Left panel: selectors */}
        <div className="w-full shrink-0 space-y-3 border-b p-3 md:w-64 md:border-b-0 md:border-r">
          <div className="space-y-1.5">
            <Label htmlFor="inspector-conversation" className="text-xs">
              Conversation
            </Label>
            <select
              id="inspector-conversation"
              className="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
              value={selectedConvId ?? ""}
              onChange={(e) => {
                setSelectedConvId(e.target.value ? Number(e.target.value) : null)
                setSelectedRunId(null)
                setCompareRunId(null)
              }}
            >
              <option value="">Choose a conversation</option>
              {conversations.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.title || `Conversation #${c.id}`}
                </option>
              ))}
            </select>
          </div>

          {selectedConvId !== null && (
            <div className="space-y-1.5">
              <Label htmlFor="inspector-run-a" className="text-xs">
                Run {mode === "compare" ? "A" : ""}
              </Label>
              <select
                id="inspector-run-a"
                className="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
                value={selectedRunId ?? ""}
                onChange={(e) => setSelectedRunId(e.target.value ? Number(e.target.value) : null)}
              >
                <option value="">Choose a run</option>
                {runs.map((r) => (
                  <option key={r.id} value={r.id}>
                    Run #{r.id} — {r.status} ({r.iterations} iter)
                  </option>
                ))}
              </select>
            </div>
          )}

          {mode === "compare" && selectedConvId !== null && (
            <div className="space-y-1.5">
              <Label htmlFor="inspector-run-b" className="text-xs">
                Run B
              </Label>
              <select
                id="inspector-run-b"
                className="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
                value={compareRunId ?? ""}
                onChange={(e) => setCompareRunId(e.target.value ? Number(e.target.value) : null)}
              >
                <option value="">Choose a second run</option>
                {runs
                  .filter((r) => r.id !== selectedRunId)
                  .map((r) => (
                    <option key={r.id} value={r.id}>
                      Run #{r.id} — {r.status} ({r.iterations} iter)
                    </option>
                  ))}
              </select>
            </div>
          )}

          {/* Replay controls (timeline mode only) */}
          {mode === "timeline" && selectedRunId !== null && (
            <div className="space-y-2 border-t pt-3">
              <Label htmlFor="inspector-replay-model" className="text-xs">
                Replay with a different model (optional)
              </Label>
              <Input
                id="inspector-replay-model"
                placeholder="e.g. gpt-4o"
                value={replayModel}
                onChange={(e) => setReplayModel(e.target.value)}
                className="h-8 text-sm"
              />
              <Button
                size="sm"
                className="w-full gap-1"
                disabled={replayMutation.isPending}
                onClick={() => replayMutation.mutate()}
              >
                {replayMutation.isPending ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Play className="h-3.5 w-3.5" />
                )}
                Start replay
              </Button>
            </div>
          )}

          {/* Run summary */}
          {timeline && (
            <RunSummaryCard timeline={timeline} />
          )}
        </div>

        {/* Main content */}
        <ScrollArea className="flex-1 p-4">
          {mode === "timeline" && (
            <>
              {timelineLoading && (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
                </div>
              )}
              {timeline && (
                <RunTimeline
                  iterations={timeline.iterations}
                  totalDurationMs={timeline.total_duration_ms}
                />
              )}
              {!timelineLoading && !timeline && (
                <p className="py-12 text-center text-sm text-muted-foreground">
                  Select a conversation and run to inspect its timeline.
                </p>
              )}
            </>
          )}

          {mode === "compare" && (
            <>
              {compareLoading && (
                <div className="flex items-center justify-center py-12">
                  <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
                </div>
              )}
              {comparison && <ComparisonView comparison={comparison} />}
              {!compareLoading && !comparison && (
                <p className="py-12 text-center text-sm text-muted-foreground">
                  Select two runs to compare them side-by-side.
                </p>
              )}
            </>
          )}
        </ScrollArea>
      </div>
    </div>
  )
}

function RunSummaryCard({ timeline }: { timeline: RunTimelineType }) {
  const { run } = timeline
  return (
    <Card className="mt-2">
      <CardHeader className="pb-2">
        <CardTitle className="text-xs text-muted-foreground">Run #{run.id} Summary</CardTitle>
      </CardHeader>
      <CardContent className="space-y-1 text-xs">
        <div className="flex justify-between">
          <span className="text-muted-foreground">Status</span>
          <Badge variant="secondary">{run.status}</Badge>
        </div>
        <div className="flex justify-between">
          <span className="text-muted-foreground">Model</span>
          <span>{run.model ?? "—"}</span>
        </div>
        <div className="flex justify-between">
          <span className="text-muted-foreground">Iterations</span>
          <span>{run.iterations}</span>
        </div>
        {run.finish_reason && (
          <div className="flex justify-between">
            <span className="text-muted-foreground">Finish</span>
            <span>{run.finish_reason}</span>
          </div>
        )}
        {run.error && (
          <div className="flex justify-between">
            <span className="text-muted-foreground">Error</span>
            <span className="max-w-[120px] truncate text-destructive">{run.error}</span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
