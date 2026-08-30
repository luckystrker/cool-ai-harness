import { useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { BarChart3 } from "lucide-react"
import { analyticsApi } from "@/api/analytics"
import type {
  AnalyticsSummary,
  CallHistoryRow,
  LatencyPoint,
  MemoryActivityPoint,
  ModelSpend,
  SpendTimeSeriesPoint,
  TopTool,
} from "@/api/types"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { QueryErrorState, QueryLoadingState } from "@/components/ui/query-state"
import { cn } from "@/lib/utils"

const fmtUsd = (n: number) =>
  n.toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    currencyDisplay: "code",
    maximumFractionDigits: 4,
  })
const fmtPercent = (n: number, maximumFractionDigits = 1) =>
  new Intl.NumberFormat(undefined, {
    style: "percent",
    maximumFractionDigits,
  }).format(n)

const DAYS_OPTIONS = [7, 14, 30, 90] as const

export function AnalyticsPage() {
  const [days, setDays] = useState<number>(30)

  const { data: summary, isLoading, isError, refetch } = useQuery({
    queryKey: ["analytics", "summary", days],
    queryFn: () => analyticsApi.summary(days),
  })

  const { data: spendTime = [] } = useQuery({
    queryKey: ["analytics", "spend-over-time", days],
    queryFn: () => analyticsApi.spendOverTime(days),
  })

  const { data: spendModel = [] } = useQuery({
    queryKey: ["analytics", "spend-by-model", days],
    queryFn: () => analyticsApi.spendByModel(days),
  })

  const { data: tools = [] } = useQuery({
    queryKey: ["analytics", "top-tools", days],
    queryFn: () => analyticsApi.topTools(days),
  })

  const { data: latency = [] } = useQuery({
    queryKey: ["analytics", "latency", days],
    queryFn: () => analyticsApi.latency(days),
  })

  const { data: callHist } = useQuery({
    queryKey: ["analytics", "call-history"],
    queryFn: () => analyticsApi.callHistory({ limit: 50 }),
  })

  const { data: memActivity = [] } = useQuery({
    queryKey: ["analytics", "memory-activity", days],
    queryFn: () => analyticsApi.memoryActivity(days),
  })

  return (
    <div className="h-full overflow-x-hidden overflow-y-auto">
      <div className="mx-auto max-w-5xl space-y-6 p-4 sm:p-6">
        {/* Header */}
        <header className="flex flex-col items-start gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <div className="hidden h-9 w-9 shrink-0 items-center justify-center rounded-md bg-primary text-primary-foreground md:flex">
              <BarChart3 className="h-4 w-4" />
            </div>
            <div>
              <h1 className="sr-only text-lg font-semibold md:not-sr-only">Analytics</h1>
              <p className="text-sm text-muted-foreground">
                Spend, tool usage, latency, and memory activity.
              </p>
            </div>
          </div>
          <div className="grid w-full grid-cols-4 gap-1 sm:w-auto" aria-label="Analytics time range">
            {DAYS_OPTIONS.map((d) => (
              <button
                key={d}
                onClick={() => setDays(d)}
                aria-label={`Show the last ${d} days`}
                aria-pressed={days === d}
                className={cn(
                  "min-h-11 rounded-md px-2.5 py-1 text-xs font-medium transition-colors sm:min-h-9",
                  days === d
                    ? "bg-primary text-primary-foreground"
                    : "bg-muted text-muted-foreground hover:bg-accent"
                )}
              >
                {d}d
              </button>
            ))}
          </div>
        </header>

        {isLoading ? (
          <QueryLoadingState label="Loading analytics…" />
        ) : isError || !summary ? (
          <QueryErrorState
            title="Analytics could not be loaded"
            description="Check that Cool is running locally, then try again."
            onRetry={() => void refetch()}
          />
        ) : (
          <>
            <SummaryCards summary={summary} />
            <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
              <SpendOverTimeCard data={spendTime} />
              <SpendByModelCard data={spendModel} />
              <LatencyCard data={latency} />
              <MemoryActivityCard data={memActivity} />
            </div>
            <TopToolsCard data={tools} />
            <CallHistoryCard data={callHist?.rows ?? []} total={callHist?.total ?? 0} />
          </>
        )}
      </div>
    </div>
  )
}

// --- Summary cards ---

function SummaryCards({ summary }: { summary: AnalyticsSummary }) {
  const cards = [
    { label: "Total spend", value: fmtUsd(summary.total_spend_usd) },
    { label: "LLM calls", value: String(summary.total_llm_calls) },
    { label: "Tokens", value: summary.total_tokens.toLocaleString() },
    { label: "Tool calls", value: String(summary.total_tool_calls) },
    {
      label: "Tool success",
      value: summary.total_tool_calls === 0 ? "—" : fmtPercent(summary.tool_success_rate),
      hint: summary.total_tool_calls === 0 ? "No samples" : undefined,
    },
  ]
  return (
    <div className="cool-event-strip grid grid-cols-2 overflow-hidden rounded-lg sm:grid-cols-5">
      {cards.map((c) => (
        <div
          key={c.label}
          className="min-h-20 border-b border-r p-3 even:border-r-0 last:col-span-2 last:border-b-0 last:border-r-0 sm:min-h-24 sm:border-b-0 sm:p-4 sm:even:border-r sm:last:col-span-1"
        >
          <p className="cool-instrument-label text-muted-foreground">{c.label}</p>
          <p className="mt-2 text-xl font-semibold tabular-nums">{c.value}</p>
          {c.hint && <p className="mt-0.5 text-[11px] text-muted-foreground">{c.hint}</p>}
        </div>
      ))}
    </div>
  )
}

function EmptyMetric({ message }: { message: string }) {
  return (
    <div className="py-5 text-center">
      <p className="text-sm font-medium">Nothing to chart yet</p>
      <p className="mx-auto mt-1 max-w-xs text-xs leading-5 text-muted-foreground">{message}</p>
    </div>
  )
}

// --- Spend over time (bar chart) ---

function SpendOverTimeCard({ data }: { data: SpendTimeSeriesPoint[] }) {
  const max = Math.max(...data.map((d) => d.cost_usd), 0.001)
  const hasSpend = data.some((point) => point.cost_usd > 0)
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">Spend over time</CardTitle>
        <CardDescription>Daily USD cost</CardDescription>
      </CardHeader>
      <CardContent>
        {!hasSpend ? (
          <EmptyMetric message="No model spend was recorded in this time range." />
        ) : (
          <div className="flex h-32 items-end gap-0.5">
            {data.map((d) => (
              <div
                key={d.period}
                className="group relative flex-1 rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                title={`${d.period}: ${fmtUsd(d.cost_usd)} (${d.calls} calls)`}
                role="img"
                tabIndex={0}
                aria-label={`${d.period}: ${fmtUsd(d.cost_usd)}, ${d.calls} calls`}
              >
                <div
                  className="w-full rounded-t bg-primary/70 transition-colors group-hover:bg-primary"
                  style={{ height: `${Math.max(2, (d.cost_usd / max) * 100)}%` }}
                />
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- Spend by model ---

function SpendByModelCard({ data }: { data: ModelSpend[] }) {
  const max = Math.max(...data.map((d) => d.cost_usd), 0.001)
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">Spend by model</CardTitle>
        <CardDescription>Cost distribution across models</CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {data.length === 0 ? (
          <EmptyMetric message="No model-level spend was recorded in this time range." />
        ) : (
          data.map((d) => (
            <div key={d.model} className="space-y-1">
              <div className="flex items-center justify-between text-xs">
                <span className="font-mono">{d.model}</span>
                <span className="tabular-nums text-muted-foreground">
                  {fmtUsd(d.cost_usd)} · {d.calls} calls
                </span>
              </div>
              <div className="h-2 w-full overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-primary/70"
                  style={{ width: `${(d.cost_usd / max) * 100}%` }}
                />
              </div>
            </div>
          ))
        )}
      </CardContent>
    </Card>
  )
}

// --- Latency ---

function LatencyCard({ data }: { data: LatencyPoint[] }) {
  const max = Math.max(...data.map((d) => d.max_ms), 1)
  const hasLatency = data.some((point) => point.avg_ms > 0)
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">LLM latency</CardTitle>
        <CardDescription>Average response time per day (ms)</CardDescription>
      </CardHeader>
      <CardContent>
        {!hasLatency ? (
          <EmptyMetric message="No completed model calls have latency samples in this time range." />
        ) : (
          <div className="flex h-32 items-end gap-0.5">
            {data.map((d) => (
              <div
                key={d.period}
                className="group relative flex-1 rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                title={`${d.period}: avg ${d.avg_ms.toFixed(0)}ms (min ${d.min_ms}, max ${d.max_ms})`}
                role="img"
                tabIndex={0}
                aria-label={`${d.period}: average ${d.avg_ms.toFixed(0)} milliseconds, minimum ${d.min_ms}, maximum ${d.max_ms}`}
              >
                <div
                  className="w-full rounded-t bg-blue-500/70 transition-colors group-hover:bg-blue-500"
                  style={{ height: `${Math.max(2, (d.avg_ms / max) * 100)}%` }}
                />
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- Memory activity ---

function MemoryActivityCard({ data }: { data: MemoryActivityPoint[] }) {
  const max = Math.max(...data.map((d) => d.created), 1)
  const hasActivity = data.some((point) => point.created > 0)
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">Memory activity</CardTitle>
        <CardDescription>New memories created per day</CardDescription>
      </CardHeader>
      <CardContent>
        {!hasActivity ? (
          <EmptyMetric message="No new memories were created in this time range." />
        ) : (
          <div className="flex h-32 items-end gap-0.5">
            {data.map((d) => (
              <div
                key={d.period}
                className="group relative flex-1 rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                title={`${d.period}: ${d.created} memories (${Object.entries(d.by_type).map(([k, v]) => `${k}:${v}`).join(", ")})`}
                role="img"
                tabIndex={0}
                aria-label={`${d.period}: ${d.created} memories created`}
              >
                <div
                  className="w-full rounded-t bg-emerald-500/70 transition-colors group-hover:bg-emerald-500"
                  style={{ height: `${Math.max(2, (d.created / max) * 100)}%` }}
                />
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- Top tools table ---

function TopToolsCard({ data }: { data: TopTool[] }) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">Top tools</CardTitle>
        <CardDescription>Most-invoked tools with success rate and latency</CardDescription>
      </CardHeader>
      <CardContent>
        {data.length === 0 ? (
          <p className="py-4 text-center text-xs text-muted-foreground">
            No tool calls were recorded in this time range.
          </p>
        ) : (
          <div className="overflow-x-auto rounded-md border">
            <table className="w-full text-xs">
              <thead className="bg-muted/50 text-muted-foreground">
                <tr>
                  <th className="px-3 py-1.5 text-left font-medium">Tool</th>
                  <th className="px-3 py-1.5 text-right font-medium">Calls</th>
                  <th className="px-3 py-1.5 text-right font-medium">Avg ms</th>
                  <th className="px-3 py-1.5 text-right font-medium">Success</th>
                  <th className="px-3 py-1.5 text-right font-medium">Errors</th>
                </tr>
              </thead>
              <tbody>
                {data.map((t) => (
                  <tr key={t.name} className="border-t">
                    <td className="px-3 py-1.5 font-mono">{t.name}</td>
                    <td className="px-3 py-1.5 text-right tabular-nums">{t.calls}</td>
                    <td className="px-3 py-1.5 text-right tabular-nums">{t.avg_duration_ms.toFixed(0)}</td>
                    <td className="px-3 py-1.5 text-right tabular-nums">
                      <span className={cn(t.success_rate < 0.9 && "text-destructive")}>
                        {fmtPercent(t.success_rate, 0)}
                      </span>
                    </td>
                    <td className="px-3 py-1.5 text-right tabular-nums">
                      {t.error_count > 0 ? (
                        <span className="text-destructive">{t.error_count}</span>
                      ) : (
                        "0"
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- Call history table ---

function CallHistoryCard({ data, total }: { data: CallHistoryRow[]; total: number }) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">LLM call history</CardTitle>
        <CardDescription>
          Unified log of all LLM calls ({total} total, showing latest {data.length})
        </CardDescription>
      </CardHeader>
      <CardContent>
        {data.length === 0 ? (
          <p className="py-4 text-center text-xs text-muted-foreground">
            No model calls were recorded yet.
          </p>
        ) : (
          <div className="max-h-72 overflow-auto rounded-md border">
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-muted/50 text-muted-foreground">
                <tr>
                  <th className="px-2 py-1.5 text-left font-medium">When</th>
                  <th className="px-2 py-1.5 text-left font-medium">Model</th>
                  <th className="px-2 py-1.5 text-left font-medium">Provider</th>
                  <th className="px-2 py-1.5 text-right font-medium">Tokens</th>
                  <th className="px-2 py-1.5 text-right font-medium">Cost</th>
                </tr>
              </thead>
              <tbody>
                {data.map((r) => (
                  <tr key={r.id} className="border-t">
                    <td className="px-2 py-1.5 text-muted-foreground">
                      {r.ts ? new Date(r.ts).toLocaleString() : "—"}
                    </td>
                    <td className="px-2 py-1.5 font-mono">{r.model || "—"}</td>
                    <td className="px-2 py-1.5">{r.provider_name || "—"}</td>
                    <td className="px-2 py-1.5 text-right tabular-nums">{r.total_tokens}</td>
                    <td className="px-2 py-1.5 text-right tabular-nums">{fmtUsd(r.cost_usd)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
