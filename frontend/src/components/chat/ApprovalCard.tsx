import { useState } from "react"
import { ShieldAlert, ShieldCheck, ShieldX, Bug, Loader2, FileDiff } from "lucide-react"
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
  /** Current file content before the write (for diff/preview). */
  currentContent?: string
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

      {/* Diff/preview for write operations (Фаза 1.5 §2) */}
      {approval.currentContent != null && approval.arguments.content != null && (
        <WriteDiffPreview
          path={String(approval.arguments.path ?? "file")}
          oldContent={approval.currentContent}
          newContent={String(approval.arguments.content)}
        />
      )}
      {approval.currentContent == null &&
        approval.arguments.content != null &&
        isWriteTool(approval.name) && (
          <NewFilePreview
            path={String(approval.arguments.path ?? "file")}
            content={String(approval.arguments.content)}
          />
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

// --- Diff/Preview helpers (Фаза 1.5 §2) ------------------------------------

const WRITE_TOOLS = new Set(["write_file"])

function isWriteTool(name: string): boolean {
  return WRITE_TOOLS.has(name)
}

interface DiffLine {
  type: "same" | "added" | "removed"
  text: string
}

/** Simple line-by-line diff (LCS-free: mark removed then added). */
function computeLineDiff(oldText: string, newText: string): DiffLine[] {
  const oldLines = oldText.split("\n")
  const newLines = newText.split("\n")
  const result: DiffLine[] = []

  // Find common prefix.
  let prefix = 0
  while (prefix < oldLines.length && prefix < newLines.length && oldLines[prefix] === newLines[prefix]) {
    result.push({ type: "same", text: oldLines[prefix] })
    prefix++
  }

  // Find common suffix.
  let suffix = 0
  while (
    suffix < oldLines.length - prefix &&
    suffix < newLines.length - prefix &&
    oldLines[oldLines.length - 1 - suffix] === newLines[newLines.length - 1 - suffix]
  ) {
    suffix++
  }

  // Removed lines (from old, not in common prefix/suffix).
  for (let i = prefix; i < oldLines.length - suffix; i++) {
    result.push({ type: "removed", text: oldLines[i] })
  }
  // Added lines (from new, not in common prefix/suffix).
  for (let i = prefix; i < newLines.length - suffix; i++) {
    result.push({ type: "added", text: newLines[i] })
  }

  // Append common suffix.
  for (let i = 0; i < suffix; i++) {
    result.push({ type: "same", text: oldLines[oldLines.length - suffix + i] })
  }

  return result
}

/** Inline diff view for overwriting an existing file. */
function WriteDiffPreview({ path, oldContent, newContent }: { path: string; oldContent: string; newContent: string }) {
  const [open, setOpen] = useState(true)
  const lines = computeLineDiff(oldContent, newContent)
  const added = lines.filter((l) => l.type === "added").length
  const removed = lines.filter((l) => l.type === "removed").length

  return (
    <div className="mt-1.5">
      <button
        className="flex items-center gap-1 text-xs text-muted-foreground underline-offset-2 hover:underline"
        onClick={() => setOpen((o) => !o)}
      >
        <FileDiff className="h-3.5 w-3.5" />
        {open ? "Hide diff" : "Show diff"} — {path}
        <span className="ml-1 text-emerald-600">+{added}</span>
        <span className="text-destructive">−{removed}</span>
      </button>
      {open && (
        <pre className="mt-1 max-h-64 overflow-auto rounded bg-muted p-2 font-mono text-[11px] leading-4">
          {lines.map((line, i) => (
            <div
              key={i}
              className={cn(
                "px-1",
                line.type === "added" && "bg-emerald-500/15 text-emerald-800",
                line.type === "removed" && "bg-destructive/10 text-destructive line-through"
              )}
            >
              <span className="mr-1 select-none text-muted-foreground">
                {line.type === "added" ? "+" : line.type === "removed" ? "−" : " "}
              </span>
              {line.text}
            </div>
          ))}
        </pre>
      )}
    </div>
  )
}

/** Preview for creating a new file (no old content to diff against). */
function NewFilePreview({ path, content }: { path: string; content: string }) {
  const [open, setOpen] = useState(false)
  const lineCount = content.split("\n").length

  return (
    <div className="mt-1.5">
      <button
        className="flex items-center gap-1 text-xs text-muted-foreground underline-offset-2 hover:underline"
        onClick={() => setOpen((o) => !o)}
      >
        <FileDiff className="h-3.5 w-3.5" />
        {open ? "Hide preview" : "Show preview"} — {path}
        <span className="ml-1 text-emerald-600">new file, {lineCount} lines</span>
      </button>
      {open && (
        <pre className="mt-1 max-h-64 overflow-auto rounded bg-muted p-2 font-mono text-[11px] leading-4">
          {content.split("\n").map((line, i) => (
            <div key={i} className="bg-emerald-500/10 px-1 text-emerald-800">
              <span className="mr-1 select-none text-muted-foreground">+</span>
              {line}
            </div>
          ))}
        </pre>
      )}
    </div>
  )
}
