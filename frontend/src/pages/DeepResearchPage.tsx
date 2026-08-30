import { useCallback, useEffect, useMemo, useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { ArrowLeft, GitCompareArrows, Loader2, Plus, SearchCheck, X } from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { deepResearchApi } from "@/api/research"
import type { ResearchRun, ResearchRunDetail } from "@/api/types"
import { ResearchCompare } from "@/components/research/ResearchCompare"
import { ResearchProgress } from "@/components/research/ResearchProgress"
import { ResearchReport } from "@/components/research/ResearchReport"
import { ResearchRunCard } from "@/components/research/ResearchRunCard"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Textarea } from "@/components/ui/textarea"
import { useResearchStream } from "@/hooks/useResearchStream"
import { useIsMobile } from "@/hooks/useMediaQuery"
import { cn } from "@/lib/utils"

export function DeepResearchPage() {
  const queryClient = useQueryClient()
  const { progress, start, abort, reset } = useResearchStream()

  const [topic, setTopic] = useState("")
  const [depth, setDepth] = useState(4)
  const [model, setModel] = useState("")
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [compareWithId, setCompareWithId] = useState<number | null>(null)
  const [rerunModel, setRerunModel] = useState("")
  const [mobilePane, setMobilePane] = useState<"list" | "detail">("list")
  const isMobile = useIsMobile()

  const { data: runs = [] } = useQuery({
    queryKey: ["research-runs"],
    queryFn: () => deepResearchApi.list(50),
    refetchInterval: (query) => {
      const active = (query.state.data as ResearchRun[] | undefined)?.some((r) =>
        ["queued", "running"].includes(r.status)
      )
      return active ? 3000 : false
    },
  })

  const { data: detail } = useQuery({
    queryKey: ["research-run", selectedId],
    queryFn: () => deepResearchApi.get(selectedId!),
    enabled: selectedId != null,
    refetchInterval: (query) => {
      const status = (query.state.data as ResearchRunDetail | undefined)?.status
      return status === "queued" || status === "running" ? 3000 : false
    },
  })

  const { data: compareDetail } = useQuery({
    queryKey: ["research-run", compareWithId],
    queryFn: () => deepResearchApi.get(compareWithId!),
    enabled: compareWithId != null,
  })

  const selected = runs.find((r) => r.id === selectedId) ?? detail ?? null
  const compareWith = runs.find((r) => r.id === compareWithId) ?? null

  // When a live (SSE) run finishes, surface it in the detail pane.
  useEffect(() => {
    if (progress.runId != null && progress.terminal) {
      setSelectedId(progress.runId)
      setMobilePane("detail")
      queryClient.invalidateQueries({ queryKey: ["research-runs"] })
    }
  }, [progress.runId, progress.terminal, queryClient])

  const cancelLive = useCallback(() => {
    if (progress.runId != null) {
      deepResearchApi.cancel(progress.runId).catch(() => undefined)
    }
    abort()
  }, [progress.runId, abort])

  const startResearch = useCallback(async () => {
    if (!topic.trim()) return
    setCompareWithId(null)
    reset()
    setRerunModel("")
    setMobilePane("detail")
    try {
      await start({ topic: topic.trim(), depth, model: model || undefined })
      toast.success("Research started")
    } catch (e) {
      setMobilePane("list")
      toast.error("Research did not start", {
        description: getErrorDescription(e, "Review the topic and model, then try again."),
      })
    }
  }, [topic, depth, model, start, reset])

  const cancelMutation = useMutation({
    mutationFn: (id: number) => deepResearchApi.cancel(id),
    onSuccess: (_, id) => {
      queryClient.invalidateQueries({ queryKey: ["research-runs"] })
      queryClient.invalidateQueries({ queryKey: ["research-run", id] })
      toast.success("Research cancelled")
    },
    onError: (error) =>
      toast.error("Research is still running", {
        description: getErrorDescription(error, "Try cancelling again."),
      }),
  })

  const rerunMutation = useMutation({
    mutationFn: (id: number) => deepResearchApi.rerun(id, { model: rerunModel || null }),
    onSuccess: (run) => {
      setCompareWithId(null)
      setSelectedId(run.id)
      setMobilePane("detail")
      queryClient.invalidateQueries({ queryKey: ["research-runs"] })
      toast.success(`Rerun started (run #${run.id})`)
    },
    onError: (error) =>
      toast.error("Research rerun did not start", {
        description: getErrorDescription(error, "Check the model override and try again."),
      }),
  })

  const compareCandidates = useMemo(
    () =>
      runs
        .filter((r) => r.status === "completed" && r.id !== selectedId)
        .slice(0, 10),
    [runs, selectedId]
  )

  const hasActive = runs.some((r) => ["queued", "running"].includes(r.status))
  const showLiveProgress = !progress.terminal && (progress.stage || progress.subQuestions.length > 0 || progress.runId != null)

  return (
    <div className="flex h-full min-w-0">
      {/* Left column: launch form + run history */}
      <div
        className={cn(
          "min-w-0 flex-1 flex-col border-r md:w-80 md:flex-none md:shrink-0",
          isMobile && mobilePane === "detail" ? "hidden" : "flex"
        )}
      >
        <div className="flex items-center justify-between border-b px-3 py-2">
          <div className="flex items-center gap-2">
            <SearchCheck className="h-4 w-4" />
            <h1 className="text-sm font-semibold">Deep Research</h1>
          </div>
          {hasActive && <Loader2 className="h-4 w-4 animate-spin text-blue-500" />}
        </div>

        <ScrollArea className="flex-1">
          <div className="space-y-3 p-3">
            {/* New research form */}
            <div className="rounded-md border p-3">
              <div className="grid gap-2.5">
                <div className="grid gap-1">
                  <Label htmlFor="research-topic">Research question</Label>
                  <Textarea
                    id="research-topic"
                    value={topic}
                    onChange={(e) => setTopic(e.target.value)}
                    placeholder="For example: How has the Alpha framework been adopted over time?"
                    rows={3}
                  />
                </div>
                <div className="grid grid-cols-2 gap-2">
                  <div className="grid gap-1">
                    <Label htmlFor="research-depth">Questions to investigate</Label>
                    <select
                      id="research-depth"
                      value={depth}
                      onChange={(e) => setDepth(parseInt(e.target.value))}
                      className="h-9 rounded-md border border-input bg-transparent px-3 text-sm shadow-sm"
                    >
                      <option value={3}>3 questions</option>
                      <option value={4}>4 questions</option>
                      <option value={5}>5 questions</option>
                    </select>
                  </div>
                  <div className="grid gap-1">
                    <Label htmlFor="research-model">Model (optional)</Label>
                    <Input
                      id="research-model"
                      value={model}
                      onChange={(e) => setModel(e.target.value)}
                      placeholder="Default"
                    />
                  </div>
                </div>
                {showLiveProgress ? (
                  <Button variant="destructive" onClick={cancelLive} className="gap-2">
                    <X className="h-4 w-4" /> Cancel research
                  </Button>
                ) : (
                  <Button
                    onClick={startResearch}
                    disabled={!topic.trim()}
                    className="gap-2"
                  >
                    <Plus className="h-4 w-4" /> Start research
                  </Button>
                )}
              </div>
            </div>

            {/* Runs */}
            <div>
              <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                Runs
              </span>
            </div>
            <ul className="space-y-1.5">
              {runs.map((run) => (
                <li key={run.id}>
                  <ResearchRunCard
                    run={run}
                    selected={run.id === selectedId}
                    onClick={() => {
                      setCompareWithId(null)
                      setSelectedId(run.id)
                      setMobilePane("detail")
                    }}
                  />
                </li>
              ))}
              {runs.length === 0 && !showLiveProgress && (
                <p className="px-1 py-4 text-center text-sm text-muted-foreground">
                  No research reports yet. Enter a question above to start one.
                </p>
              )}
            </ul>
          </div>
        </ScrollArea>
      </div>

      {/* Right column: progress / report / compare */}
      <div
        className={cn(
          "min-w-0 flex-1 flex-col",
          isMobile && mobilePane === "list" ? "hidden" : "flex"
        )}
      >
        <div className="flex h-12 shrink-0 items-center border-b px-2 md:hidden">
          <Button
            variant="ghost"
            className="h-11 gap-2 px-2"
            onClick={() => setMobilePane("list")}
          >
            <ArrowLeft className="h-4 w-4" />
            Research runs
          </Button>
        </div>
        {showLiveProgress ? (
          <div className="flex-1 overflow-y-auto p-4">
            <div className="mb-3 flex items-center gap-2">
              <Loader2 className="h-4 w-4 animate-spin text-blue-500" />
              <span className="text-sm font-semibold">Researching: {topic}</span>
            </div>
            <ResearchProgress progress={progress} />
          </div>
        ) : compareWith && selected ? (
          <div className="flex min-h-0 flex-1 flex-col">
            <div className="flex flex-col gap-2 border-b px-4 py-2 sm:flex-row sm:items-center sm:justify-between">
              <div className="flex items-center gap-2 text-sm font-semibold">
                <GitCompareArrows className="h-4 w-4" /> Comparison
              </div>
              <div className="flex min-w-0 items-center gap-2">
                <Label htmlFor="research-compare-run" className="text-xs text-muted-foreground">
                  vs
                </Label>
                <select
                  id="research-compare-run"
                  value={compareWith.id}
                  onChange={(e) => setCompareWithId(parseInt(e.target.value))}
                  className="h-8 rounded-md border border-input bg-transparent px-2 text-sm"
                >
                  {compareCandidates.map((r) => (
                    <option key={r.id} value={r.id}>
                      #{r.id} {r.model ?? "default"} · {r.sources_count} sources
                    </option>
                  ))}
                </select>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => setCompareWithId(null)}
                  aria-label="Close comparison"
                >
                  <X className="h-4 w-4" />
                </Button>
              </div>
            </div>
            {detail && compareDetail && <ResearchCompare runs={[detail, compareDetail]} />}
          </div>
        ) : selected ? (
          <div className="flex min-h-0 flex-1 flex-col">
            <div className="flex flex-col gap-2 border-b px-4 py-2 sm:flex-row sm:items-center sm:justify-between">
              <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs text-muted-foreground">
                <span>run #{selected.id}</span>
                <span>·</span>
                <span>{selected.model ?? "default model"}</span>
                <span>·</span>
                <span>{new Date(selected.created_at).toLocaleString()}</span>
                {selected.usage?.total_tokens != null && (
                  <>
                    <span>·</span>
                    <span>{selected.usage.total_tokens} tokens</span>
                  </>
                )}
                {selected.usage?.cost_usd != null && (
                  <>
                    <span>·</span>
                    <span>${selected.usage.cost_usd.toFixed(4)}</span>
                  </>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1.5">
                {(selected.status === "queued" || selected.status === "running") && (
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={() => selected.id != null && cancelMutation.mutate(selected.id)}
                  >
                    Cancel research
                  </Button>
                )}
                {selected.status === "completed" && (
                  <>
                    <Input
                      aria-label="Model override for rerun"
                      value={rerunModel}
                      onChange={(e) => setRerunModel(e.target.value)}
                      placeholder="Optional model for rerun"
                      className="h-11 min-w-0 flex-1 sm:h-8 sm:w-40 sm:flex-none"
                    />
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={rerunMutation.isPending}
                      onClick={() => selected.id != null && rerunMutation.mutate(selected.id)}
                    >
                      {rerunMutation.isPending ? "Starting rerun…" : "Rerun research"}
                    </Button>
                    {compareCandidates.length > 0 && (
                      <Button
                        variant="outline"
                        size="sm"
                        className="gap-1.5"
                        onClick={() => {
                          if (!compareWithId && compareCandidates.length > 0) {
                            setCompareWithId(compareCandidates[0].id)
                          }
                        }}
                      >
                        <GitCompareArrows className="h-3.5 w-3.5" /> Compare
                      </Button>
                    )}
                  </>
                )}
              </div>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto p-4">
              {selected.status === "completed" && detail ? (
                <ResearchReport run={detail} />
              ) : selected.status === "failed" ? (
                <p className="text-sm text-red-500">
                  Research failed: {selected.error ?? "unknown error"}
                </p>
              ) : selected.status === "cancelled" ? (
                <p className="text-sm text-muted-foreground">Research was cancelled.</p>
              ) : (
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" /> Running in the background…
                </div>
              )}
            </div>
          </div>
        ) : (
          <div className="flex flex-1 items-center justify-center p-8 text-center text-sm text-muted-foreground">
            <div className="max-w-sm space-y-2">
              <SearchCheck className="mx-auto h-8 w-8 opacity-40" />
              <p>
                Deep research decomposes a topic into sub-questions, runs parallel
                researcher agents, and synthesizes a cited report with a bibliography.
              </p>
              <p className="text-xs">Pick a run on the left or start a new one.</p>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
