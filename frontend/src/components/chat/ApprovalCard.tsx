import { useState } from "react"
import { ShieldAlert, ShieldCheck, ShieldX, Bug, Loader2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

/** Approval request rendered inline in the chat flow (replaces the modal dialog). */
export interface InlineApproval {
  callId: string
  name: string
  arguments: Record<string, unknown>
  reason: string
  /** True when triggered by a breakpoint (vs a regular "ask" tool). */
  isBreakpoint?: boolean
  /** Breakpoint type, when isBreakpoint is true. */
  breakpointType?: string
  /** Result preview (for after_tool_result breakpoints). */
  resultPreview?: string
  /** Lifecycle: waiting for the user → resolving → resolved outcome. */
  status: "pending" | "resolving" | "approved" | "denied" | "timed_out"
}

interface ApprovalCardProps {
  approval: InlineApproval
  onRespond: (approved: boolean) => void
}

/**
 * Inline card shown in the message flow when the agent wants to run a tool
 * gated behind an "ask" permission or a breakpoint. The agent loop is blocked
 * server-side until the user decides — the card renders Allow / Deny buttons
 * directly in the chat instead of a modal popup.
 */
export function ApprovalCard({ approval, onRespond }: ApprovalCardProps) {
  const [argsOpen, setArgsOpen] = useState(false)
  const isBreakpoint = approval.isBreakpoint ?? false
  const hasArgs = Object.keys(approval.arguments ?? {}).length > 0
  const resolved = approval.status !== "pending" && approval.status !== "resolving"

  return (
    <div
      className={cn(
        "rounded-lg border px-3 py-2.5 text-sm",
        resolved
          ? approval.status === "approved"
            ? "border-emerald-500/40 bg-emerald-500/5"
            : "border-destructive/30 bg-destructive/5"
          : "border-amber-500/50 bg-amber-500/5"
      )}
    >
      {/* Header */}
      <div className="flex items-center gap-2">
        {resolved ? (
          approval.status === "approved" ? (
            <ShieldCheck className="h-4 w-4 shrink-0 text-emerald-500" />
          ) : (
            <ShieldX className="h-4 w-4 shrink-0 text-destructive" />
          )
        ) : isBreakpoint ? (
          <Bug className="h-4 w-4 shrink-0 text-blue-500" />
        ) : (
          <ShieldAlert className="h-4 w-4 shrink-0 text-amber-500" />
        )}
        <span className="font-medium">
          {resolved
            ? approval.status === "approved"
              ? "Approved"
              : approval.status === "timed_out"
                ? "Timed out — denied"
                : "Denied"
            : isBreakpoint
              ? `Breakpoint: ${approval.breakpointType ?? "pause"}`
              : "Approval required"}
        </span>
        <span className="font-mono text-xs text-muted-foreground">{approval.name}</span>
      </div>

      {/* Reason / description */}
      {!resolved && (
        <p className="mt-1 text-xs text-muted-foreground">
          {isBreakpoint
            ? `A ${approval.breakpointType ?? ""} breakpoint fired. Review before proceeding.`
            : approval.reason || "The agent wants to run a tool that requires your approval."}
        </p>
      )}

      {/* Arguments (collapsible) */}
      {hasArgs && (
        <div className="mt-1.5">
          <button
            className="text-xs text-muted-foreground underline-offset-2 hover:underline"
            onClick={() => setArgsOpen((o) => !o)}
          >
            {argsOpen ? "Hide arguments" : "Show arguments"}
          </button>
          {argsOpen && (
            <pre className="mt-1 max-h-48 overflow-auto rounded bg-muted p-2 font-mono text-[11px]">
              {JSON.stringify(approval.arguments, null, 2)}
            </pre>
          )}
        </div>
      )}

      {/* Result preview (after_tool_result breakpoints) */}
      {approval.resultPreview && (
        <div className="mt-1.5">
          <div className="mb-0.5 text-xs text-muted-foreground">Result preview</div>
          <pre className="max-h-48 overflow-auto rounded bg-muted p-2 font-mono text-[11px]">
            {approval.resultPreview}
          </pre>
        </div>
      )}

      {/* Action buttons / resolved badge */}
      <div className="mt-2 flex items-center gap-2">
        {resolved ? (
          <span
            className={cn(
              "text-xs font-medium",
              approval.status === "approved" ? "text-emerald-600" : "text-destructive"
            )}
          >
            {approval.status === "approved"
              ? "✓ Allowed — continuing…"
              : "✗ Blocked — the agent was notified."}
          </span>
        ) : approval.status === "resolving" ? (
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Loader2 className="h-3.5 w-3.5 animate-spin" /> Sending decision…
          </span>
        ) : (
          <>
            <Button size="sm" className="h-7 px-3 text-xs" onClick={() => onRespond(true)}>
              Allow
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 border-destructive/40 px-3 text-xs text-destructive hover:bg-destructive/10 hover:text-destructive"
              onClick={() => onRespond(false)}
            >
              Deny
            </Button>
          </>
        )}
      </div>
    </div>
  )
}
