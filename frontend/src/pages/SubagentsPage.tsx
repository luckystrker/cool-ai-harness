import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { ArrowLeft, Bot, Pencil, Plus, Trash2 } from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { conversationsApi } from "@/api/conversations"
import { subagentsApi } from "@/api/subagents"
import type { SubagentRole } from "@/api/types"
import { RoleEditor } from "@/components/subagents/RoleEditor"
import { RunCard } from "@/components/subagents/RunCard"
import { LaunchForm } from "@/components/subagents/LaunchForm"
import { Button } from "@/components/ui/button"
import { ScrollArea } from "@/components/ui/scroll-area"
import { useIsMobile } from "@/hooks/useMediaQuery"
import { cn } from "@/lib/utils"

type Tab = "roles" | "launch" | "monitor"

/** Shared role list: desktop left panel + mobile Roles tab. Actions are
 *  hover-revealed on desktop but always visible on touch devices. */
function RoleList({
  roles,
  selectedId,
  onSelect,
  onDelete,
}: {
  roles: SubagentRole[]
  selectedId: number | null
  onSelect: (role: SubagentRole) => void
  onDelete: (id: number) => void
}) {
  return (
    <ul className="space-y-0.5 p-2">
      {roles.map((role) => (
        <li
          key={role.id}
          className={cn(
            "group flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-muted",
            selectedId === role.id && "bg-muted"
          )}
        >
          <button className="min-w-0 flex-1 text-left" onClick={() => onSelect(role)}>
            <span className="truncate font-medium">{role.name}</span>
            {role.is_builtin && (
              <span className="ml-1.5 rounded bg-muted px-1 py-0.5 text-[10px] text-muted-foreground">
                builtin
              </span>
            )}
            {role.description && (
              <p className="truncate text-xs text-muted-foreground">{role.description}</p>
            )}
          </button>
          <div className="flex shrink-0 gap-0.5 md:opacity-0 md:transition-opacity md:group-hover:opacity-100">
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8 md:h-5 md:w-5"
              onClick={() => onSelect(role)}
              title="Edit"
            >
              <Pencil className="h-3.5 w-3.5 md:h-3 md:w-3" />
            </Button>
            {!role.is_builtin && (
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8 md:h-5 md:w-5"
                onClick={() => onDelete(role.id)}
                title="Delete"
              >
                <Trash2 className="h-3.5 w-3.5 md:h-3 md:w-3" />
              </Button>
            )}
          </div>
        </li>
      ))}
      {roles.length === 0 && (
        <li className="px-2 py-3 text-center text-xs text-muted-foreground">
          No custom roles yet. Create one to reuse a model, prompt, and tool limits.
        </li>
      )}
    </ul>
  )
}

export function SubagentsPage() {
  const queryClient = useQueryClient()
  const isMobile = useIsMobile()
  const [tab, setTab] = useState<Tab>("monitor")
  const [editingRole, setEditingRole] = useState<SubagentRole | null>(null)
  const [showEditor, setShowEditor] = useState(false)

  const { data: roles = [] } = useQuery({
    queryKey: ["subagent-roles"],
    queryFn: subagentsApi.listRoles,
  })

  const { data: runs = [] } = useQuery({
    queryKey: ["subagent-runs"],
    queryFn: () => subagentsApi.listRuns(),
    refetchInterval: 3000,
  })

  // Fetch conversations to get a valid parent for standalone launches.
  const { data: conversations = [] } = useQuery({
    queryKey: ["conversations"],
    queryFn: conversationsApi.list,
  })

  const deleteRoleMutation = useMutation({
    mutationFn: (id: number) => subagentsApi.deleteRole(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["subagent-roles"] })
      toast.success("Role deleted")
    },
    onError: (error) =>
      toast.error("Subagent role was not deleted", {
        description: getErrorDescription(error, "Refresh the role list and try again."),
      }),
  })

  const openNewRole = () => {
    setEditingRole(null)
    setShowEditor(true)
    setTab("roles")
  }

  const openRole = (role: SubagentRole) => {
    setEditingRole(role)
    setShowEditor(true)
    setTab("roles")
  }

  // Only show standalone launches (not chat-spawned subagents with parent_run_id).
  const standaloneRuns = runs.filter((r) => r.parent_run_id == null)
  const activeRuns = standaloneRuns.filter((r) => r.status === "queued" || r.status === "running")
  const pastRuns = standaloneRuns.filter((r) => ["completed", "failed", "cancelled"].includes(r.status))

  // Use the first available conversation as parent for standalone launches.
  // Prefer the parent from existing runs; fall back to the first conversation.
  const parentConvId =
    runs[0]?.parent_conversation_id ?? conversations[0]?.id ?? null

  const roleList = (
    <RoleList
      roles={roles}
      selectedId={editingRole?.id ?? null}
      onSelect={openRole}
      onDelete={(id) => deleteRoleMutation.mutate(id)}
    />
  )

  return (
    <div className="flex h-full flex-col md:flex-row">
      {/* Left panel: roles + run history (desktop only — on mobile these live
          inside the Roles / Monitor tabs). */}
      <div className="hidden w-72 shrink-0 flex-col border-r md:flex">
        <div className="flex items-center justify-between border-b px-3 py-2">
          <div className="flex items-center gap-2">
            <Bot className="h-4 w-4" />
            <span className="text-sm font-semibold">Subagents</span>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={openNewRole}
            title="Create subagent role"
            aria-label="Create subagent role"
          >
            <Plus className="h-4 w-4" />
          </Button>
        </div>

        <ScrollArea className="flex-1">
          {/* Roles section */}
          <div className="px-3 pt-2">
            <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
              Roles
            </span>
          </div>
          {roleList}

          {/* Run history */}
          <div className="px-3 pt-3">
            <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
              Recent Runs
            </span>
          </div>
          <div className="space-y-1.5 p-2">
            {standaloneRuns.slice(0, 20).map((run) => (
              <RunCard key={run.id} run={run} />
            ))}
            {standaloneRuns.length === 0 && (
              <p className="px-2 py-3 text-center text-xs text-muted-foreground">
                No standalone subagent runs yet.
              </p>
            )}
          </div>
        </ScrollArea>
      </div>

      {/* Content panel with the tab bar */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        {/* Tab bar — M3-style tabs, full-width and touch-friendly on mobile */}
        <div className="flex shrink-0 border-b">
          {(["monitor", "launch", "roles"] as Tab[]).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={cn(
                "min-h-11 flex-1 px-4 text-sm font-medium capitalize transition-colors hover:text-foreground md:flex-none",
                tab === t
                  ? "border-b-2 border-primary text-foreground"
                  : "text-muted-foreground"
              )}
            >
              {t}
            </button>
          ))}
        </div>

        <ScrollArea className="flex-1">
          {tab === "monitor" && (
            <div className="p-4">
              <h3 className="mb-3 text-sm font-semibold">Active subagents</h3>
              {activeRuns.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No active subagents. Launch one from the Launch tab.
                </p>
              ) : (
                <div className="grid gap-2">
                  {activeRuns.map((run) => (
                    <RunCard key={run.id} run={run} />
                  ))}
                </div>
              )}

              {pastRuns.length > 0 && (
                <>
                  <h3 className="mb-3 mt-6 text-sm font-semibold">History</h3>
                  <div className="grid gap-2">
                    {pastRuns.map((run) => (
                      <RunCard key={run.id} run={run} />
                    ))}
                  </div>
                </>
              )}
            </div>
          )}

          {tab === "launch" &&
            (parentConvId != null ? (
              <LaunchForm parentConversationId={parentConvId} />
            ) : (
              <div className="p-4 text-sm text-muted-foreground">
                No conversations available. Create a conversation first to launch
                subagents.
              </div>
            ))}

          {/* Mobile Roles tab: list ↔ editor navigation with a back action. */}
          {tab === "roles" && isMobile && (
            <>
              {showEditor ? (
                <div>
                  <div className="flex items-center gap-1 border-b px-2 py-1">
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-10 w-10"
                      onClick={() => {
                        setShowEditor(false)
                        setEditingRole(null)
                      }}
                      title="Back to roles"
                      aria-label="Back to roles"
                    >
                      <ArrowLeft className="h-4 w-4" />
                    </Button>
                    <span className="truncate text-sm font-semibold">
                      {editingRole ? editingRole.name : "New Role"}
                    </span>
                  </div>
                  <RoleEditor
                    key={editingRole?.id ?? "new"}
                    role={editingRole}
                    onSaved={() => {
                      setShowEditor(false)
                      setEditingRole(null)
                    }}
                  />
                </div>
              ) : (
                <div>
                  <div className="flex items-center justify-between border-b px-3 py-2">
                    <span className="text-sm font-semibold">Roles</span>
                    <Button
                      variant="outline"
                      size="sm"
                      className="gap-1.5"
                      onClick={openNewRole}
                    >
                      <Plus className="h-4 w-4" /> New role
                    </Button>
                  </div>
                  {roleList}
                </div>
              )}
            </>
          )}

          {/* Desktop Roles tab: editor only (list is in the left panel). */}
          {tab === "roles" && !isMobile && showEditor && (
            <RoleEditor
              key={editingRole?.id ?? "new"}
              role={editingRole}
              onSaved={() => {
                setShowEditor(false)
                setEditingRole(null)
              }}
            />
          )}
          {tab === "roles" && !isMobile && !showEditor && (
            <div className="p-4 text-sm text-muted-foreground">
              Select a role from the left panel to edit it, or use the plus button to create one.
            </div>
          )}
        </ScrollArea>
      </div>
    </div>
  )
}
