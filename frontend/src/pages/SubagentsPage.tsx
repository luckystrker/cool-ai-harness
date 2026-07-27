import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Bot, Pencil, Plus, Trash2 } from "lucide-react"
import { toast } from "sonner"
import { subagentsApi } from "@/api/subagents"
import type { SubagentRole } from "@/api/types"
import { RoleEditor } from "@/components/subagents/RoleEditor"
import { RunCard } from "@/components/subagents/RunCard"
import { LaunchForm } from "@/components/subagents/LaunchForm"
import { Button } from "@/components/ui/button"
import { ScrollArea } from "@/components/ui/scroll-area"
import { cn } from "@/lib/utils"

type Tab = "roles" | "launch" | "monitor"

export function SubagentsPage() {
  const queryClient = useQueryClient()
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

  const deleteRoleMutation = useMutation({
    mutationFn: (id: number) => subagentsApi.deleteRole(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["subagent-roles"] })
      toast.success("Role deleted")
    },
    onError: (e) => toast.error("Failed to delete role", { description: String(e) }),
  })

  // Only show standalone launches (not chat-spawned subagents with parent_run_id).
  const standaloneRuns = runs.filter((r) => r.parent_run_id == null)
  const activeRuns = standaloneRuns.filter((r) => r.status === "queued" || r.status === "running")
  const pastRuns = standaloneRuns.filter((r) => ["completed", "failed", "cancelled"].includes(r.status))

  // Use conversation_id=1 as the default parent for standalone launches.
  const parentConvId = runs[0]?.parent_conversation_id ?? 1

  return (
    <div className="flex h-full">
      {/* Left panel: roles + run history */}
      <div className="flex w-72 shrink-0 flex-col border-r">
        <div className="flex items-center justify-between border-b px-3 py-2">
          <div className="flex items-center gap-2">
            <Bot className="h-4 w-4" />
            <span className="text-sm font-semibold">Subagents</span>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={() => {
              setEditingRole(null)
              setShowEditor(true)
              setTab("roles")
            }}
            title="New Role"
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
          <ul className="space-y-0.5 p-2">
            {roles.map((role: SubagentRole) => (
              <li
                key={role.id}
                className={cn(
                  "group flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-muted",
                  editingRole?.id === role.id && "bg-muted"
                )}
              >
                <button
                  className="min-w-0 flex-1 text-left"
                  onClick={() => {
                    setEditingRole(role)
                    setShowEditor(true)
                    setTab("roles")
                  }}
                >
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
                <div className="flex shrink-0 gap-0.5 opacity-0 group-hover:opacity-100">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-5 w-5"
                    onClick={() => {
                      setEditingRole(role)
                      setShowEditor(true)
                      setTab("roles")
                    }}
                  >
                    <Pencil className="h-3 w-3" />
                  </Button>
                  {!role.is_builtin && (
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-5 w-5"
                      onClick={() => deleteRoleMutation.mutate(role.id)}
                    >
                      <Trash2 className="h-3 w-3" />
                    </Button>
                  )}
                </div>
              </li>
            ))}
            {roles.length === 0 && (
              <li className="px-2 py-3 text-center text-xs text-muted-foreground">
                No roles defined yet.
              </li>
            )}
          </ul>

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
                No runs yet.
              </p>
            )}
          </div>
        </ScrollArea>
      </div>

      {/* Right panel: tabbed content */}
      <div className="flex min-w-0 flex-1 flex-col">
        {/* Tab bar */}
        <div className="flex border-b">
          {(["monitor", "launch", "roles"] as Tab[]).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={cn(
                "px-4 py-2 text-sm font-medium capitalize transition-colors hover:text-foreground",
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
              <h3 className="mb-3 text-sm font-semibold">Active Subagents</h3>
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

          {tab === "launch" && <LaunchForm parentConversationId={parentConvId} />}

          {tab === "roles" && showEditor && (
            <RoleEditor
              key={editingRole?.id ?? "new"}
              role={editingRole}
              onSaved={() => {
                setShowEditor(false)
                setEditingRole(null)
              }}
            />
          )}
          {tab === "roles" && !showEditor && (
            <div className="p-4 text-sm text-muted-foreground">
              Select a role from the left panel to edit, or click + to create a new one.
            </div>
          )}
        </ScrollArea>
      </div>
    </div>
  )
}
