import { useQuery } from "@tanstack/react-query"
import { Wallet } from "lucide-react"
import { useState } from "react"
import { budgetsApi } from "@/api/budgets"
import { cn } from "@/lib/utils"

const fmtUsd = (n: number) =>
  n.toLocaleString(undefined, { style: "currency", currency: "USD", maximumFractionDigits: 4 })

/**
 * A compact budget indicator shown in the chat header. Hover or click reveals
 * a popover with the configured limits and current spend per window (Фаза 1.5 §5).
 * Color reflects status: green (ok), amber (alert), red (blocked).
 */
export function BudgetIndicator() {
  const [open, setOpen] = useState(false)
  const { data: status, isLoading } = useQuery({
    queryKey: ["budgets"],
    queryFn: budgetsApi.getStatus,
    // Refresh periodically so live spend stays current while chatting.
    refetchInterval: 15_000,
  })

  if (!status) {
    // No data yet (or no limits configured) — render a neutral icon.
    return (
      <div
        className="relative inline-flex"
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
      >
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          title="Cost budget"
        >
          <Wallet className="h-4 w-4" />
        </button>
        {open && <Popover isLoading={isLoading} status={null} />}
      </div>
    )
  }

  const tone =
    status.status === "blocked" ? "text-destructive"
    : status.status === "alert" ? "text-warning"
    : "text-muted-foreground"

  return (
    <div
      className="relative inline-flex"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className={cn(
          "inline-flex items-center gap-1 rounded-md p-1.5 hover:bg-accent hover:text-foreground",
          tone
        )}
        title={`Cost budget: ${status.status}`}
      >
        <Wallet className="h-4 w-4" />
        {status.status !== "ok" && (
          <span className="h-1.5 w-1.5 rounded-full bg-current" />
        )}
      </button>
      {open && <Popover isLoading={isLoading} status={status} />}
    </div>
  )
}

function Popover({
  isLoading,
  status,
}: {
  isLoading: boolean
  status: import("@/api/types").BudgetStatusResponse | null
}) {
  return (
    <div className="absolute right-0 top-full z-50 mt-1 w-64 rounded-md border bg-popover p-3 text-xs shadow-md">
      {isLoading || !status ? (
        <div className="text-muted-foreground">Loading budget…</div>
      ) : (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="font-medium">Cost budget</span>
            <StatusBadge status={status.status} />
          </div>
          <Row label="Today" spend={status.daily} />
          <Row label="This week" spend={status.weekly} />
          <Row label="This month" spend={status.monthly} />
          {status.overridden && (
            <p className="pt-1 text-[11px] text-muted-foreground">
              A block override is active.
            </p>
          )}
          <a
            href="/budgets"
            className="block pt-1 text-[11px] text-primary hover:underline"
          >
            Manage budgets →
          </a>
        </div>
      )}
    </div>
  )
}

function Row({ label, spend }: { label: string; spend: import("@/api/types").BudgetWindowSpend }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="text-muted-foreground">{label}</span>
      <span className="tabular-nums">
        {fmtUsd(spend.spend_usd)}
        {spend.limit_usd !== null && (
          <span className="text-muted-foreground"> / {fmtUsd(spend.limit_usd)}</span>
        )}
      </span>
    </div>
  )
}

function StatusBadge({ status }: { status: string }) {
  const cls =
    status === "blocked" ? "bg-destructive/15 text-destructive"
    : status === "alert" ? "bg-warning/15 text-warning"
    : "bg-muted text-muted-foreground"
  return (
    <span className={cn("rounded px-1.5 py-0.5 text-[10px] font-medium capitalize", cls)}>
      {status}
    </span>
  )
}
