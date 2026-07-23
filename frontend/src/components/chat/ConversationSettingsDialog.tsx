import { useEffect, useState } from "react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import type {
  BreakpointConfig,
  BreakpointType,
  CapabilityPolicy,
  ToolPermission,
  ToolPermissions,
} from "@/api/types"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { cn } from "@/lib/utils"

// Tools the agent exposes (kept in sync with backend register_builtins()).
// The "*" entry is the wildcard default applied to any tool not listed.
const TOOL_NAMES = [
  "*",
  "read_file",
  "write_file",
  "list_files",
  "python_execute",
  "web_search",
  "web_fetch",
] as const

const CAPABILITY_NAMES = [
  "*",
  "read",
  "write",
  "execute",
  "network",
  "git",
  "send_external",
] as const

const BREAKPOINT_TYPES: { type: BreakpointType; label: string; hint: string }[] = [
  { type: "before_tool", label: "Before tool", hint: "Pause before any tool call" },
  { type: "before_write", label: "Before write", hint: "Pause before file writes" },
  { type: "after_tool_result", label: "After result", hint: "Pause after a tool returns" },
  { type: "before_send", label: "Before send", hint: "Pause before sending to LLM" },
]

const PERMISSIONS: ToolPermission[] = ["allow", "ask", "deny"]

const PERM_STYLES: Record<ToolPermission, string> = {
  allow: "bg-emerald-500/15 text-emerald-600",
  ask: "bg-amber-500/15 text-amber-600",
  deny: "bg-red-500/15 text-red-600",
}

/**
 * Quick permission presets. Selecting one writes a permission map that
 * expresses the chosen posture; the user can then fine-tune individual tools
 * in the matrix below. "allow edits" runs file/list tools freely but still
 * confirms before executing code (the riskiest built-in).
 */
type PermissionMode = "ask" | "allow" | "allow_edits"

const MODE_PRESETS: Record<PermissionMode, ToolPermissions> = {
  ask: { "*": "ask" },
  allow: { "*": "allow" },
  allow_edits: { "*": "allow", python_execute: "ask" },
}

const MODE_LABELS: { mode: PermissionMode; label: string; hint: string }[] = [
  { mode: "ask", label: "Always ask", hint: "Confirm every tool call" },
  { mode: "allow_edits", label: "Allow edits", hint: "Free files; confirm code" },
  { mode: "allow", label: "Always allow", hint: "Run everything without asking" },
]

/** Derive the active preset from a permission map (for highlighting). */
function modeFromPerms(perms: ToolPermissions): PermissionMode | null {
  for (const [mode, preset] of Object.entries(MODE_PRESETS) as [PermissionMode, ToolPermissions][]) {
    const keys = Object.keys(preset)
    if (
      keys.length === Object.keys(perms).length &&
      keys.every((k) => perms[k] === preset[k])
    ) {
      return mode
    }
  }
  return null
}

interface ConversationSettingsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  conversationId: number
  workingDirectory: string | null
  permissions: ToolPermissions | null
  capabilityPolicy: CapabilityPolicy | null
  breakpoints: BreakpointConfig[] | null
  onSaved: () => void
}

/**
 * Per-conversation settings: working directory and a tool-permission matrix.
 * Patches the conversation via PATCH /api/conversations/{id}.
 */
export function ConversationSettingsDialog({
  open,
  onOpenChange,
  conversationId,
  workingDirectory,
  permissions,
  capabilityPolicy,
  breakpoints,
  onSaved,
}: ConversationSettingsDialogProps) {
  // Remount-driven form seed: parent controls `open`, and we sync from props
  // whenever the dialog opens so edits always start from the persisted state.
  const [workdir, setWorkdir] = useState(workingDirectory ?? "")
  const [perms, setPerms] = useState<ToolPermissions>(permissions ?? {})
  const [capPolicy, setCapPolicy] = useState<CapabilityPolicy>(capabilityPolicy ?? {})
  const [bpList, setBpList] = useState<BreakpointConfig[]>(breakpoints ?? [])
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    if (open) {
      setWorkdir(workingDirectory ?? "")
      setPerms(permissions ?? {})
      setCapPolicy(capabilityPolicy ?? {})
      setBpList(breakpoints ?? [])
    }
  }, [open, workingDirectory, permissions, capabilityPolicy, breakpoints])

  const cycle = (tool: string) => {
    setPerms((cur) => {
      const current = (cur[tool] ?? "ask") as ToolPermission
      const nextIdx = (PERMISSIONS.indexOf(current) + 1) % PERMISSIONS.length
      const next = PERMISSIONS[nextIdx]
      return { ...cur, [tool]: next }
    })
  }

  const cycleCap = (cap: string) => {
    setCapPolicy((cur) => {
      const current = (cur[cap] ?? "allow") as ToolPermission
      const nextIdx = (PERMISSIONS.indexOf(current) + 1) % PERMISSIONS.length
      const next = PERMISSIONS[nextIdx]
      return { ...cur, [cap]: next }
    })
  }

  const toggleBreakpoint = (type: BreakpointType) => {
    setBpList((cur) => {
      const exists = cur.some((bp) => bp.type === type)
      if (exists) {
        return cur.filter((bp) => bp.type !== type)
      }
      return [...cur, { type, fallback: "deny" as const }]
    })
  }

  const handleSave = async () => {
    setSaving(true)
    try {
      await conversationsApi.update(conversationId, {
        working_directory: workdir.trim() || undefined,
        permissions: perms,
        capability_policy: capPolicy,
        breakpoints: bpList,
      })
      onSaved()
      onOpenChange(false)
      toast.success("Conversation settings saved")
    } catch (e) {
      toast.error("Failed to save settings", { description: String(e) })
    } finally {
      setSaving(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Conversation settings</DialogTitle>
          <DialogDescription>
            Override the working directory and per-tool permissions for this
            conversation. These take precedence over the global defaults.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="workdir">Working directory</Label>
            <Input
              id="workdir"
              value={workdir}
              onChange={(e) => setWorkdir(e.target.value)}
              placeholder="(use global default)"
              className="font-mono text-xs"
            />
            <p className="text-xs text-muted-foreground">
              File tools confine paths here; subprocesses run with this as cwd.
            </p>
          </div>

          <div className="space-y-2">
            <Label>Tool permissions</Label>
            <div className="grid grid-cols-3 gap-2">
              {MODE_LABELS.map(({ mode, label, hint }) => {
                const active = modeFromPerms(perms) === mode
                return (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => setPerms({ ...MODE_PRESETS[mode] })}
                    className={cn(
                      "rounded-md border px-2 py-1.5 text-left transition-colors",
                      active
                        ? "border-primary bg-primary/10"
                        : "hover:bg-muted"
                    )}
                  >
                    <div className="text-xs font-medium">{label}</div>
                    <div className="text-[10px] text-muted-foreground">{hint}</div>
                  </button>
                )
              })}
            </div>
            <p className="text-xs text-muted-foreground">
              Pick a preset, then fine-tune below. Click a cell to cycle:
              allow → ask → deny. The “*” row is the default for any tool not
              listed.
            </p>
            <div className="rounded-md border">
              {TOOL_NAMES.map((tool, i) => {
                const value = (perms[tool] ?? (tool === "*" ? "ask" : "inherit")) as
                  | ToolPermission
                  | "inherit"
                return (
                  <div
                    key={tool}
                    className={cn(
                      "flex items-center justify-between px-3 py-2",
                      i > 0 && "border-t"
                    )}
                  >
                    <span className="font-mono text-xs">{tool}</span>
                    <button
                      type="button"
                      onClick={() => cycle(tool)}
                      className={cn(
                        "rounded px-2 py-0.5 text-xs font-medium capitalize transition-colors",
                        value === "inherit"
                          ? "bg-muted text-muted-foreground"
                          : PERM_STYLES[value]
                      )}
                    >
                      {value}
                    </button>
                  </div>
                )
              })}
            </div>
          </div>

          {/* Capability policy */}
          <div className="space-y-2">
            <Label>Capability policy</Label>
            <p className="text-xs text-muted-foreground">
              Coarse-grained gates applied before per-tool permissions. The more
              restrictive of the two layers wins. Click to cycle:
              allow → ask → deny.
            </p>
            <div className="rounded-md border">
              {CAPABILITY_NAMES.map((cap, i) => {
                const value = (capPolicy[cap] ?? (cap === "*" ? "allow" : "inherit")) as
                  | ToolPermission
                  | "inherit"
                return (
                  <div
                    key={cap}
                    className={cn(
                      "flex items-center justify-between px-3 py-2",
                      i > 0 && "border-t"
                    )}
                  >
                    <span className="font-mono text-xs">{cap}</span>
                    <button
                      type="button"
                      onClick={() => cycleCap(cap)}
                      className={cn(
                        "rounded px-2 py-0.5 text-xs font-medium capitalize transition-colors",
                        value === "inherit"
                          ? "bg-muted text-muted-foreground"
                          : PERM_STYLES[value]
                      )}
                    >
                      {value}
                    </button>
                  </div>
                )
              })}
            </div>
          </div>

          {/* Breakpoints */}
          <div className="space-y-2">
            <Label>Breakpoints</Label>
            <p className="text-xs text-muted-foreground">
              Pause the agent at key points for human review. Toggle on to
              enable; the agent blocks until you approve or the timeout fires.
            </p>
            <div className="grid grid-cols-2 gap-2">
              {BREAKPOINT_TYPES.map(({ type, label, hint }) => {
                const active = bpList.some((bp) => bp.type === type)
                return (
                  <button
                    key={type}
                    type="button"
                    onClick={() => toggleBreakpoint(type)}
                    className={cn(
                      "rounded-md border px-2 py-1.5 text-left transition-colors",
                      active
                        ? "border-primary bg-primary/10"
                        : "hover:bg-muted"
                    )}
                  >
                    <div className="text-xs font-medium">{label}</div>
                    <div className="text-[10px] text-muted-foreground">{hint}</div>
                  </button>
                )
              })}
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={handleSave} disabled={saving}>
            {saving ? "Saving…" : "Save"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
