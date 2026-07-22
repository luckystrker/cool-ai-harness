import { useEffect, useState } from "react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import type { ToolPermission, ToolPermissions } from "@/api/types"
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

const PERMISSIONS: ToolPermission[] = ["allow", "ask", "deny"]

const PERM_STYLES: Record<ToolPermission, string> = {
  allow: "bg-emerald-500/15 text-emerald-600",
  ask: "bg-amber-500/15 text-amber-600",
  deny: "bg-red-500/15 text-red-600",
}

interface ConversationSettingsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  conversationId: number
  workingDirectory: string | null
  permissions: ToolPermissions | null
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
  onSaved,
}: ConversationSettingsDialogProps) {
  // Remount-driven form seed: parent controls `open`, and we sync from props
  // whenever the dialog opens so edits always start from the persisted state.
  const [workdir, setWorkdir] = useState(workingDirectory ?? "")
  const [perms, setPerms] = useState<ToolPermissions>(permissions ?? {})
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    if (open) {
      setWorkdir(workingDirectory ?? "")
      setPerms(permissions ?? {})
    }
  }, [open, workingDirectory, permissions])

  const cycle = (tool: string) => {
    setPerms((cur) => {
      const current = (cur[tool] ?? "ask") as ToolPermission
      const nextIdx = (PERMISSIONS.indexOf(current) + 1) % PERMISSIONS.length
      const next = PERMISSIONS[nextIdx]
      return { ...cur, [tool]: next }
    })
  }

  const handleSave = async () => {
    setSaving(true)
    try {
      await conversationsApi.update(conversationId, {
        working_directory: workdir.trim() || undefined,
        permissions: perms,
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
            <p className="text-xs text-muted-foreground">
              Click a cell to cycle: allow → ask → deny. The “*” row is the
              default for any tool not listed.
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
