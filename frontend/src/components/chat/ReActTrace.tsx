import { useState } from "react"
import { Brain, Zap, Eye, ChevronRight, ChevronDown } from "lucide-react"
import { cn } from "@/lib/utils"
import type { ReActStep } from "@/api/types"

export interface ReActTraceProps {
  /** ReAct steps accumulated during the agent loop. */
  steps: ReActStep[]
  /** True while the assistant is still streaming. */
  streaming?: boolean
}

/**
 * Renders the ReAct (Thought → Action → Observation) trace as a collapsible
 * timeline. Each step shows the model's reasoning, the tool it invoked, and
 * the result it observed. Auto-expanded while streaming, collapsed once done.
 */
export function ReActTrace({ steps, streaming }: ReActTraceProps) {
  const [open, setOpen] = useState(true)

  if (steps.length === 0) return null

  return (
    <div className="rounded-lg border border-violet-500/20 bg-violet-500/5 text-xs">
      <button
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-muted-foreground"
        onClick={() => setOpen((o) => !o)}
      >
        {open ? (
          <ChevronDown className="h-3.5 w-3.5" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5" />
        )}
        <Brain className={cn("h-3.5 w-3.5 text-violet-500", streaming && "animate-pulse")} />
        <span className="font-medium text-violet-600 dark:text-violet-400">
          ReAct Trace
        </span>
        <span className="ml-auto font-mono text-[11px] text-muted-foreground/70">
          {steps.length} step{steps.length !== 1 ? "s" : ""}
        </span>
      </button>

      {open && (
        <div className="border-t border-violet-500/15 px-3 py-2">
          <div className="relative ml-2 space-y-0 border-l border-violet-500/20 pl-4">
            {steps.map((step, idx) => (
              <ReActStepRow
                key={step.step}
                step={step}
                isLast={idx === steps.length - 1}
                streaming={streaming}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

function ReActStepRow({
  step,
  isLast,
  streaming,
}: {
  step: ReActStep
  isLast: boolean
  streaming?: boolean
}) {
  const [expanded, setExpanded] = useState(true)
  const isActive = streaming && isLast

  return (
    <div className={cn("relative pb-3", isLast && "pb-0")}>
      {/* Step number badge on the timeline */}
      <div
        className={cn(
          "absolute -left-[21px] top-0.5 flex h-4 w-4 items-center justify-center rounded-full text-[9px] font-bold",
          isActive
            ? "bg-violet-500 text-white animate-pulse"
            : "bg-violet-500/20 text-violet-600 dark:text-violet-400"
        )}
      >
        {step.step}
      </div>

      <button
        className="flex w-full items-center gap-1.5 text-left"
        onClick={() => setExpanded((o) => !o)}
      >
        {expanded ? (
          <ChevronDown className="h-3 w-3 text-muted-foreground/60" />
        ) : (
          <ChevronRight className="h-3 w-3 text-muted-foreground/60" />
        )}
        <span className="font-medium text-foreground/80">Step {step.step}</span>
        {step.actions.length > 0 && (
          <span className="ml-auto font-mono text-[10px] text-muted-foreground/60">
            {step.actions.map((a) => a.tool_name).join(", ")}
          </span>
        )}
      </button>

      {expanded && (
        <div className="mt-1.5 space-y-1.5">
          {/* Thought */}
          {step.thought && (
            <div className="flex items-start gap-1.5">
              <Brain className="mt-0.5 h-3 w-3 shrink-0 text-amber-500" />
              <div className="min-w-0">
                <span className="font-semibold text-amber-600 dark:text-amber-400">Thought: </span>
                <span className="text-muted-foreground">
                  {step.thought.length > 200
                    ? step.thought.slice(0, 200) + "…"
                    : step.thought}
                </span>
              </div>
            </div>
          )}

          {/* Actions */}
          {step.actions.map((action, i) => (
            <div key={i} className="flex items-start gap-1.5">
              <Zap className="mt-0.5 h-3 w-3 shrink-0 text-blue-500" />
              <div className="min-w-0">
                <span className="font-semibold text-blue-600 dark:text-blue-400">Action: </span>
                <span className="font-mono text-[11px] text-foreground/70">
                  {action.tool_name}
                </span>
                {Object.keys(action.arguments).length > 0 && (
                  <span className="ml-1 text-muted-foreground/60">
                    ({Object.entries(action.arguments)
                      .slice(0, 3)
                      .map(([k, v]) => `${k}=${JSON.stringify(v).slice(0, 30)}`)
                      .join(", ")}
                    {Object.keys(action.arguments).length > 3 ? ", …" : ""})
                  </span>
                )}
              </div>
            </div>
          ))}

          {/* Observations */}
          {step.observations.map((obs, i) => (
            <div key={i} className="flex items-start gap-1.5">
              <Eye className={cn("mt-0.5 h-3 w-3 shrink-0", obs.is_error ? "text-red-500" : "text-emerald-500")} />
              <div className="min-w-0">
                <span
                  className={cn(
                    "font-semibold",
                    obs.is_error
                      ? "text-red-600 dark:text-red-400"
                      : "text-emerald-600 dark:text-emerald-400"
                  )}
                >
                  Observation:{" "}
                </span>
                <span className="text-muted-foreground">
                  {obs.result_summary.length > 150
                    ? obs.result_summary.slice(0, 150) + "…"
                    : obs.result_summary || "(no output)"}
                </span>
              </div>
            </div>
          ))}

          {/* Loading indicator for active step */}
          {isActive && step.observations.length === 0 && step.actions.length > 0 && (
            <div className="flex items-center gap-1.5 text-muted-foreground/60">
              <Eye className="h-3 w-3 animate-pulse" />
              <span className="italic">Observing…</span>
            </div>
          )}
        </div>
      )}
    </div>
  )
}
