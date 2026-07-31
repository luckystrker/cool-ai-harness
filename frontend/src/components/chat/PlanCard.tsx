import { useState } from "react"
import {
  CheckCircle2,
  Circle,
  Clock,
  ListChecks,
  Loader2,
  Play,
  SkipForward,
  XCircle,
} from "lucide-react"
import type { Plan, PlanStep, PlanStepStatus } from "@/api/types"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

interface PlanCardProps {
  plan: Plan
  onApprove?: (approved: boolean) => void
  onExecute?: () => void
  /** True while the plan execution is streaming. */
  executing?: boolean
}

const stepStatusIcon: Record<PlanStepStatus, typeof Circle> = {
  pending: Circle,
  running: Loader2,
  completed: CheckCircle2,
  failed: XCircle,
  skipped: SkipForward,
}

const stepStatusColor: Record<PlanStepStatus, string> = {
  pending: "text-muted-foreground",
  running: "text-blue-500 animate-spin",
  completed: "text-green-500",
  failed: "text-red-500",
  skipped: "text-yellow-500",
}

export function PlanCard({ plan, onApprove, onExecute, executing }: PlanCardProps) {
  const [collapsed, setCollapsed] = useState(false)
  const isDraft = plan.status === "draft"
  const isApproved = plan.status === "approved"
  const isExecuting = plan.status === "executing" || executing
  const isTerminal = ["completed", "failed", "cancelled"].includes(plan.status)

  const completedCount = plan.steps.filter((s) => s.status === "completed").length
  const progressPct = plan.steps.length > 0 ? (completedCount / plan.steps.length) * 100 : 0

  return (
    <div className="my-2 rounded-lg border bg-card p-3 shadow-sm">
      {/* Header */}
      <div className="flex items-center gap-2">
        <ListChecks className="h-4 w-4 shrink-0 text-primary" />
        <span className="font-medium">{plan.title || "Plan"}</span>
        <StatusBadge status={plan.status} />
        <button
          className="ml-auto text-xs text-muted-foreground hover:text-foreground"
          onClick={() => setCollapsed((v) => !v)}
        >
          {collapsed ? "Show" : "Hide"}
        </button>
      </div>

      {/* Progress bar (when executing or terminal) */}
      {(isExecuting || isTerminal) && (
        <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-muted">
          <div
            className={cn(
              "h-full rounded-full transition-all",
              plan.status === "failed" ? "bg-red-500" : "bg-green-500"
            )}
            style={{ width: `${progressPct}%` }}
          />
        </div>
      )}

      {/* Steps */}
      {!collapsed && (
        <ol className="mt-3 space-y-1.5">
          {plan.steps.map((step) => (
            <StepRow key={step.position} step={step} />
          ))}
        </ol>
      )}

      {/* Actions */}
      {isDraft && onApprove && (
        <div className="mt-3 flex gap-2">
          <Button size="sm" onClick={() => onApprove(true)}>
            <Play className="mr-1 h-3 w-3" /> Approve & Execute
          </Button>
          <Button size="sm" variant="outline" onClick={() => onApprove(false)}>
            Reject
          </Button>
        </div>
      )}
      {isApproved && onExecute && (
        <div className="mt-3">
          <Button size="sm" onClick={onExecute}>
            <Play className="mr-1 h-3 w-3" /> Execute Plan
          </Button>
        </div>
      )}
    </div>
  )
}

function StepRow({ step }: { step: PlanStep }) {
  const Icon = stepStatusIcon[step.status] || Circle
  const color = stepStatusColor[step.status] || "text-muted-foreground"

  return (
    <li className="flex items-start gap-2 text-sm">
      <Icon className={cn("mt-0.5 h-4 w-4 shrink-0", color)} />
      <div className="min-w-0">
        <span className={cn(step.status === "completed" && "text-muted-foreground line-through")}>
          {step.title}
        </span>
        {step.description && (
          <p className="text-xs text-muted-foreground">{step.description}</p>
        )}
        {step.result_summary && (
          <p className="mt-0.5 text-xs text-muted-foreground italic">
            {step.result_summary}
          </p>
        )}
        {step.depends_on && step.depends_on.length > 0 && (
          <p className="text-xs text-muted-foreground">
            Depends on: {step.depends_on.map((d) => `#${d + 1}`).join(", ")}
          </p>
        )}
        {step.delegate_role && (
          <p className="text-xs text-purple-600 dark:text-purple-400">
            Delegated to: {step.delegate_role}
          </p>
        )}
      </div>
    </li>
  )
}

function StatusBadge({ status }: { status: Plan["status"] }) {
  const styles: Record<string, string> = {
    draft: "bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200",
    approved: "bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200",
    executing: "bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200",
    completed: "bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200",
    failed: "bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200",
    cancelled: "bg-gray-100 text-gray-800 dark:bg-gray-800 dark:text-gray-200",
  }
  const icons: Record<string, typeof Clock> = {
    draft: Clock,
    approved: CheckCircle2,
    executing: Loader2,
    completed: CheckCircle2,
    failed: XCircle,
    cancelled: XCircle,
  }
  const Icon = icons[status] || Clock
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium",
        styles[status] || styles.draft
      )}
    >
      <Icon className={cn("h-3 w-3", status === "executing" && "animate-spin")} />
      {status}
    </span>
  )
}
