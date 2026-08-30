import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Wallet, Loader2, ShieldOff, ShieldCheck, AlertTriangle } from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { budgetsApi } from "@/api/budgets"
import type { BudgetStatusResponse, BudgetWindow, BudgetWindowSpend } from "@/api/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { QueryErrorState, QueryLoadingState } from "@/components/ui/query-state"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { cn } from "@/lib/utils"

const fmtUsd = (n: number) =>
  n.toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    currencyDisplay: "code",
    maximumFractionDigits: 4,
  })

export function BudgetsPage() {
  const queryClient = useQueryClient()

  const { data: status, isLoading, isError, refetch } = useQuery({
    queryKey: ["budgets"],
    queryFn: budgetsApi.getStatus,
  })

  const { data: spend = [], isLoading: spendLoading } = useQuery({
    queryKey: ["budgets", "spend"],
    queryFn: () => budgetsApi.spend({ limit: 100 }),
  })

  const updateMutation = useMutation({
    mutationFn: budgetsApi.update,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["budgets"] })
      toast.success("Budget updated")
    },
    onError: (error) =>
      toast.error("Budget limits were not saved", {
        description: getErrorDescription(error, "Review the limits and try again."),
      }),
  })

  const overrideMutation = useMutation({
    mutationFn: (until: string) => budgetsApi.setOverride(until),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["budgets"] })
      toast.success("Block override active")
    },
    onError: (error) =>
      toast.error("Budget block was not lifted", {
        description: getErrorDescription(error, "Check the duration and try again."),
      }),
  })

  const clearOverrideMutation = useMutation({
    mutationFn: budgetsApi.clearOverride,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["budgets"] })
      toast.success("Block override cleared")
    },
    onError: (error) =>
      toast.error("Budget override was not cleared", {
        description: getErrorDescription(error, "Refresh the budget status and try again."),
      }),
  })

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl space-y-6 p-6">
        <header className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <Wallet className="h-4 w-4" />
          </div>
          <div>
            <h1 className="text-lg font-semibold">Budgets</h1>
            <p className="text-sm text-muted-foreground">
               Set daily, weekly, and monthly spending limits. Cool warns at
              the threshold and can block new model calls after a limit is reached.
            </p>
          </div>
        </header>

        {isLoading ? (
          <QueryLoadingState label="Loading budget status…" />
        ) : isError || !status ? (
          <QueryErrorState
            title="Budget status could not be loaded"
            description="Check that Cool is running locally, then try again."
            onRetry={() => void refetch()}
          />
        ) : (
          <>
            <StatusBanner status={status} />

            <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
              <WindowCard label="Today" window={status.daily} />
              <WindowCard label="This week" window={status.weekly} />
              <WindowCard label="This month" window={status.monthly} />
            </div>

            <BudgetForm
              status={status}
              pending={updateMutation.isPending}
              onSubmit={(body) => updateMutation.mutate(body)}
            />

            <OverrideControls
              status={status}
              onOverride={(until) => overrideMutation.mutate(until)}
              onClear={() => clearOverrideMutation.mutate()}
              overridePending={overrideMutation.isPending}
              clearPending={clearOverrideMutation.isPending}
            />

            <SpendHistory rows={spend} loading={spendLoading} />
          </>
        )}
      </div>
    </div>
  )
}

function StatusBanner({ status }: { status: BudgetStatusResponse }) {
  if (status.status === "blocked") {
    return (
      <Card className="border-destructive/40 bg-destructive/5">
        <CardContent className="flex items-center gap-3 py-3">
          <AlertTriangle className="h-5 w-5 text-destructive" />
          <div className="text-sm">
            <strong>Spending limit reached.</strong> New model calls are blocked.
            {status.overridden && " A temporary override is active, so calls are allowed for now."}
          </div>
        </CardContent>
      </Card>
    )
  }
  if (status.status === "alert") {
    return (
      <Card className="border-warning/40 bg-warning/5">
        <CardContent className="flex items-center gap-3 py-3">
          <AlertTriangle className="h-5 w-5 text-warning" />
          <div className="text-sm">
            Spending has crossed the alert threshold ({status.alert_threshold_pct}%).
          </div>
        </CardContent>
      </Card>
    )
  }
  return null
}

function WindowCard({ label, window: w }: { label: string; window: BudgetWindowSpend }) {
  const hasLimit = w.limit_usd !== null
  const pct = Math.min(100, w.pct)
  const tone =
    !hasLimit ? "bg-muted-foreground/30"
    : w.pct >= 100 ? "bg-destructive"
    : w.pct >= 80 ? "bg-warning"
    : "bg-primary"
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardDescription>{label}</CardDescription>
        <CardTitle className="text-xl">{fmtUsd(w.spend_usd)}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2">
        <div className="h-2 w-full overflow-hidden rounded-full bg-muted">
          <div className={cn("h-full transition-all", tone)} style={{ width: `${hasLimit ? pct : 0}%` }} />
        </div>
        <div className="text-xs text-muted-foreground">
          {hasLimit ? (
            <>of {fmtUsd(w.limit_usd!)} · {w.pct.toFixed(0)}%</>
          ) : (
            <Badge variant="outline">no limit set</Badge>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

function BudgetForm({
  status,
  pending,
  onSubmit,
}: {
  status: BudgetStatusResponse
  pending: boolean
  onSubmit: (body: {
    daily_limit_usd: number | null
    weekly_limit_usd: number | null
    monthly_limit_usd: number | null
    alert_threshold_pct: number
    block_on_exceed: boolean
  }) => void
}) {
  const [daily, setDaily] = useState(status.daily_limit_usd?.toString() ?? "")
  const [weekly, setWeekly] = useState(status.weekly_limit_usd?.toString() ?? "")
  const [monthly, setMonthly] = useState(status.monthly_limit_usd?.toString() ?? "")
  const [threshold, setThreshold] = useState(status.alert_threshold_pct.toString())
  const [block, setBlock] = useState(status.block_on_exceed)

  const parseLimit = (s: string): number | null => {
    const t = s.trim()
    if (t === "") return null
    const n = Number(t)
    return Number.isFinite(n) ? n : null
  }

  const limitEntries = [
    ["Daily", daily],
    ["Weekly", weekly],
    ["Monthly", monthly],
  ] as const
  const invalidLimit = limitEntries.find(([, raw]) => {
    if (raw.trim() === "") return false
    const value = Number(raw)
    return !Number.isFinite(value) || value < 0
  })
  const parsedThreshold = Number(threshold)
  const thresholdInvalid =
    threshold.trim() === "" ||
    !Number.isFinite(parsedThreshold) ||
    parsedThreshold < 0 ||
    parsedThreshold > 100
  const formError = invalidLimit
    ? `${invalidLimit[0]} limit must be zero or greater.`
    : thresholdInvalid
      ? "Alert threshold must be between 0 and 100."
      : null
  const dirty =
    parseLimit(daily) !== status.daily_limit_usd ||
    parseLimit(weekly) !== status.weekly_limit_usd ||
    parseLimit(monthly) !== status.monthly_limit_usd ||
    (!thresholdInvalid && parsedThreshold !== status.alert_threshold_pct) ||
    block !== status.block_on_exceed

  const handleSubmit = () => {
    if (formError) return
    onSubmit({
      daily_limit_usd: parseLimit(daily),
      weekly_limit_usd: parseLimit(weekly),
      monthly_limit_usd: parseLimit(monthly),
      alert_threshold_pct: parsedThreshold,
      block_on_exceed: block,
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Limits</CardTitle>
        <CardDescription>
          Leave a field blank to disable that window. The alert threshold and
          block behavior apply to all windows.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <div className="space-y-1.5">
            <Label htmlFor="b-daily">Daily (USD)</Label>
             <Input id="b-daily" type="number" min="0" step="0.01" placeholder="—" value={daily} onChange={(e) => setDaily(e.target.value)} aria-invalid={invalidLimit?.[0] === "Daily"} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="b-weekly">Weekly (USD)</Label>
             <Input id="b-weekly" type="number" min="0" step="0.01" placeholder="—" value={weekly} onChange={(e) => setWeekly(e.target.value)} aria-invalid={invalidLimit?.[0] === "Weekly"} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="b-monthly">Monthly (USD)</Label>
             <Input id="b-monthly" type="number" min="0" step="0.01" placeholder="—" value={monthly} onChange={(e) => setMonthly(e.target.value)} aria-invalid={invalidLimit?.[0] === "Monthly"} />
          </div>
        </div>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="b-threshold">Alert threshold (%)</Label>
             <Input id="b-threshold" type="number" min="0" max="100" step="1" value={threshold} onChange={(e) => setThreshold(e.target.value)} aria-invalid={thresholdInvalid} />
          </div>
          <div className="flex items-end">
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={block} onChange={(e) => setBlock(e.target.checked)} className="h-4 w-4 rounded border-input" />
              Block calls when exceeded
            </label>
          </div>
        </div>
        <div className="flex flex-col gap-3 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-h-5 text-xs" role="status" aria-live="polite">
            {formError ? (
              <span className="text-destructive">{formError}</span>
            ) : dirty ? (
              <span className="text-muted-foreground">
                Ready to save · alert at {parsedThreshold}% · blocking {block ? "on" : "off"}
              </span>
            ) : (
              <span className="text-muted-foreground">No unsaved changes.</span>
            )}
          </div>
          <Button onClick={handleSubmit} disabled={pending || !!formError || !dirty} className="gap-1.5">
            {pending && <Loader2 className="h-4 w-4 animate-spin" />}
            Save limits
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

function OverrideControls({
  status,
  onOverride,
  onClear,
  overridePending,
  clearPending,
}: {
  status: BudgetStatusResponse
  onOverride: (until: string) => void
  onClear: () => void
  overridePending: boolean
  clearPending: boolean
}) {
  const [hours, setHours] = useState("1")
  const [confirmOpen, setConfirmOpen] = useState(false)
  const parsedHours = Number(hours)
  const hoursInvalid =
    hours.trim() === "" || !Number.isFinite(parsedHours) || parsedHours < 1 || parsedHours > 168
  const canOverride = status.status === "blocked" && status.block_on_exceed && !status.overridden
  const affectedWindows = [
    status.daily.limit_usd !== null ? "daily" : null,
    status.weekly.limit_usd !== null ? "weekly" : null,
    status.monthly.limit_usd !== null ? "monthly" : null,
  ].filter(Boolean)
  const untilPreview = hoursInvalid
    ? null
    : new Date(Date.now() + parsedHours * 3600_000)
  const handleOverride = () => {
    if (hoursInvalid) return
    const until = new Date(Date.now() + parsedHours * 3600_000).toISOString()
    onOverride(until)
    setConfirmOpen(false)
  }

  return (
    <Card className="border-warning/45 bg-warning/5">
      <CardHeader>
        <CardTitle className="text-base">Temporarily allow model calls</CardTitle>
        <CardDescription>
          Pause budget blocking for a fixed time. When the override expires,
          Cool applies the spending limits again. Use this only to finish work you have already
          reviewed.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-wrap items-end gap-3">
        <div className="space-y-1.5">
          <Label htmlFor="o-hours">Duration (hours)</Label>
          <Input id="o-hours" type="number" min="1" max="168" step="1" value={hours} onChange={(e) => setHours(e.target.value)} className="w-32" aria-invalid={hoursInvalid} />
        </div>
        <Button onClick={() => setConfirmOpen(true)} disabled={overridePending || hoursInvalid || !canOverride} variant="warning" className="gap-1.5">
          {overridePending ? <Loader2 className="h-4 w-4 animate-spin" /> : <ShieldOff className="h-4 w-4" />}
          Allow calls temporarily
        </Button>
        <Button onClick={onClear} disabled={clearPending || !status.overridden} variant="outline" className="gap-1.5">
          {clearPending ? <Loader2 className="h-4 w-4 animate-spin" /> : <ShieldCheck className="h-4 w-4" />}
          End override now
        </Button>
        {status.override_until && (
          <span className="text-xs text-muted-foreground">
            Active until {new Date(status.override_until).toLocaleString()}
          </span>
        )}
        {!status.overridden && !canOverride && (
          <span className="w-full text-xs text-muted-foreground">
            Available only after an enabled spending limit blocks model calls.
          </span>
        )}
      </CardContent>
      <Dialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Temporarily lift the spending block?</DialogTitle>
            <DialogDescription>
              Model calls will be allowed until {untilPreview?.toLocaleString() ?? "the selected time"}.
              {affectedWindows.length > 0
                ? ` This suspends the ${affectedWindows.join(", ")} limit${affectedWindows.length === 1 ? "" : "s"}.`
                : " Your configured limits remain stored."}
            </DialogDescription>
          </DialogHeader>
          <div className="cool-event-strip rounded-md p-3 text-sm">
            <div className="cool-instrument-label text-warning">Authority change</div>
            <p className="mt-1">Cool will continue recording cost while enforcement is paused.</p>
          </div>
          <DialogFooter className="gap-2 sm:space-x-0">
            <Button variant="outline" onClick={() => setConfirmOpen(false)}>Keep block active</Button>
            <Button variant="warning" onClick={handleOverride} disabled={overridePending}>
              {overridePending && <Loader2 className="h-4 w-4 animate-spin" />}
              Lift block for {parsedHours}h
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}

function SpendHistory({ rows, loading }: { rows: import("@/api/types").SpendRow[]; loading: boolean }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Spend history</CardTitle>
        <CardDescription>The most recent LLM calls (newest first).</CardDescription>
      </CardHeader>
      <CardContent>
        {loading ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : rows.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">
            No model-call spending has been recorded yet.
          </p>
        ) : (
          <div className="max-h-80 overflow-y-auto rounded-md border">
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-muted/50 text-muted-foreground">
                <tr>
                  <th className="px-2 py-1.5 text-left font-medium">When</th>
                  <th className="px-2 py-1.5 text-left font-medium">Model</th>
                  <th className="px-2 py-1.5 text-right font-medium">Tokens</th>
                  <th className="px-2 py-1.5 text-right font-medium">Cost</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((r) => (
                  <tr key={r.id} className="border-t">
                    <td className="px-2 py-1.5 text-muted-foreground">
                      {new Date(r.ts).toLocaleString()}
                    </td>
                    <td className="px-2 py-1.5 font-mono">{r.model || "—"}</td>
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

// Re-export the window type for callers (BudgetIndicator).
export type { BudgetWindow }
