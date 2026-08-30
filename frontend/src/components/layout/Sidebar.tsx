import { useEffect, useMemo, useState } from "react"
import { useLocation, useNavigate, useParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  ChevronRight,
  FolderOpen,
  Globe,
  Loader2,
  MessageSquare,
  Plus,
  Search,
  Settings2,
  Trash2,
} from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import type { Conversation } from "@/api/types"
import { loadAgentDefaults, loadLastModel } from "@/lib/agentConfig"
import { NAV_GROUPS } from "@/lib/nav"
import {
  deleteProject,
  loadConversationProjectMap,
  loadProjects,
  setConversationProject,
  type Project,
} from "@/lib/projects"
import { ProjectDialog } from "@/components/chat/ProjectDialog"
import { ProjectSettingsDialog } from "@/components/chat/ProjectSettingsDialog"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import { QueryErrorState } from "@/components/ui/query-state"
import { cn } from "@/lib/utils"

const CONVERSATION_BATCH_SIZE = 60

export function Sidebar({
  className,
  inDrawer = false,
}: { className?: string; inDrawer?: boolean } = {}) {
  const navigate = useNavigate()
  const location = useLocation()
  const { conversationId } = useParams()
  const queryClient = useQueryClient()

  const { data: conversations = [], isLoading, isError, refetch } = useQuery({
    queryKey: ["conversations"],
    queryFn: conversationsApi.list,
  })

  // Projects live in localStorage; mirror them into state so the list re-renders
  // after the dialog creates one or the trash icon removes one.
  const [projects, setProjects] = useState<Project[]>(() => loadProjects())
  const [convProjectMap, setConvProjectMap] = useState<Record<string, string>>(() =>
    loadConversationProjectMap()
  )
  const [projectDialogOpen, setProjectDialogOpen] = useState(false)
  const [settingsProject, setSettingsProject] = useState<Project | null>(null)
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set())
  const [searchQuery, setSearchQuery] = useState("")
  const [visibleConversationLimit, setVisibleConversationLimit] = useState(
    CONVERSATION_BATCH_SIZE
  )
  const [deleteTarget, setDeleteTarget] = useState<
    | { kind: "conversation"; conversation: Conversation }
    | { kind: "project"; project: Project }
    | null
  >(null)
  const activeNavGroup = NAV_GROUPS.find((group) =>
    group.items.some((item) => location.pathname.startsWith(item.to))
  )?.id
  const [expandedNavGroup, setExpandedNavGroup] = useState<string | null>(
    activeNavGroup ?? null
  )

  useEffect(() => {
    if (activeNavGroup) setExpandedNavGroup(activeNavGroup)
  }, [activeNavGroup])

  useEffect(() => {
    setVisibleConversationLimit(CONVERSATION_BATCH_SIZE)
  }, [searchQuery])

  const createMutation = useMutation({
    mutationFn: conversationsApi.create,
    onSuccess: (conv) => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      navigate(`/chat/${conv.id}`)
    },
    onError: () => toast.error("Conversation could not be created", {
      description: "Check that the local harness is running, then try again.",
    }),
  })

  const deleteMutation = useMutation({
    mutationFn: conversationsApi.delete,
    onSuccess: (_data, deletedId) => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      if (Number(conversationId) === deletedId) navigate("/")
      setDeleteTarget(null)
      toast.success("Conversation deleted")
    },
    onError: () => toast.error("Conversation was not deleted", {
      description: "Refresh the list and try again. Your conversation is still available.",
    }),
  })

  // Create a new chat inside an existing project: pin the project's folder,
  // carry over the last-selected model + agent defaults, and link the two.
  const createInProjectMutation = useMutation({
    mutationFn: async (project: Project) => {
      const defaults = loadAgentDefaults()
      const lastModel = loadLastModel()
      const conv = await conversationsApi.create({
        ...(project.type === "local" && project.path
          ? { working_directory: project.path }
          : {}),
        ...(lastModel ? { model: lastModel } : {}),
        permissions: defaults.permissions,
        capability_policy: defaults.capabilityPolicy,
        breakpoints: defaults.breakpoints,
      })
      setConversationProject(conv.id, project.id)
      return conv
    },
    onSuccess: (conv, project) => {
      setConvProjectMap(loadConversationProjectMap())
      // Reveal the new chat by expanding its project.
      setCollapsed((prev) => {
        const next = new Set(prev)
        next.delete(project.id)
        return next
      })
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      navigate(`/chat/${conv.id}`)
    },
    onError: () => toast.error("Project conversation could not be created", {
      description: "Check that the local harness is running, then try again.",
    }),
  })

  const handleCreate = () => {
    // Apply the global agent defaults (Settings → Agent) to new conversations,
    // and carry over the last model the user picked so it persists across chats.
    const defaults = loadAgentDefaults()
    const lastModel = loadLastModel()
    createMutation.mutate({
      ...(lastModel ? { model: lastModel } : {}),
      permissions: defaults.permissions,
      capability_policy: defaults.capabilityPolicy,
      breakpoints: defaults.breakpoints,
    })
  }
  const handleDeleteProject = (project: Project) => {
    setProjects(deleteProject(project.id))
    setConvProjectMap(loadConversationProjectMap())
    setDeleteTarget(null)
    toast.success("Project removed", {
      description: "Its conversations are still available.",
    })
  }

  const toggleCollapsed = (id: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  // Split conversations into per-project buckets + an unassigned list.
  const { byProject, unassigned, visibleProjects } = useMemo(() => {
    const map = new Map<string, Conversation[]>()
    const rest: Conversation[] = []
    const query = searchQuery.trim().toLocaleLowerCase()
    const matchesConversation = (conversation: Conversation) =>
      !query ||
      (conversation.title || `Conversation #${conversation.id}`)
        .toLocaleLowerCase()
        .includes(query)
    for (const c of conversations) {
      const pid = convProjectMap[String(c.id)]
      if (pid && projects.some((p) => p.id === pid)) {
        const project = projects.find((p) => p.id === pid)
        if (matchesConversation(c) || project?.name.toLocaleLowerCase().includes(query)) {
          if (!map.has(pid)) map.set(pid, [])
          map.get(pid)!.push(c)
        }
      } else if (matchesConversation(c)) {
        rest.push(c)
      }
    }
    const filteredProjects = projects.filter(
      (project) =>
        !query ||
        project.name.toLocaleLowerCase().includes(query) ||
        (map.get(project.id)?.length ?? 0) > 0
    )
    return { byProject: map, unassigned: rest, visibleProjects: filteredProjects }
  }, [conversations, convProjectMap, projects, searchQuery])

  const renderConversationRow = (c: Conversation) => {
    const active = Number(conversationId) === c.id
    return (
      <li key={c.id}>
        <div
          className={cn(
            "group flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors",
            active ? "bg-accent text-accent-foreground" : "hover:bg-accent/60",
            inDrawer && "min-h-11"
          )}
        >
          <button
            className="flex flex-1 items-center gap-2 overflow-hidden text-left"
            onClick={() => navigate(`/chat/${c.id}`)}
          >
            <MessageSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
            <span className="truncate">{c.title || `Conversation #${c.id}`}</span>
          </button>
          <button
            className={cn(
              "grid shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive focus-visible:opacity-100",
              inDrawer
                ? "h-11 w-11"
                : "h-7 w-7 opacity-0 group-hover:opacity-100"
            )}
            title={`Delete ${c.title || `Conversation #${c.id}`}`}
            aria-label={`Delete ${c.title || `Conversation #${c.id}`}`}
            onClick={() => setDeleteTarget({ kind: "conversation", conversation: c })}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </div>
      </li>
    )
  }

  return (
    <aside
      className={cn("flex shrink-0 flex-col border-r bg-muted/30", className ?? "w-72")}
    >
      {/* Brand */}
      <div className="flex h-14 items-center gap-2 border-b px-4">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <span className="text-sm font-bold">H</span>
        </div>
        <span className="font-semibold tracking-tight">Harness</span>
      </div>

      {/* Pinned section nav (mobile drawer) — Material navigation drawers keep
          destinations fixed at the top; the conversation list scrolls below. */}
      {inDrawer && (
        <NavigationGroups
          expanded={expandedNavGroup}
          onExpandedChange={setExpandedNavGroup}
          pathname={location.pathname}
          onNavigate={navigate}
          className="border-b px-2 py-2"
        />
      )}

      {/* New chat — creates a conversation immediately, no title prompt.
          The title can be edited later via the conversation settings. */}
      <div className="p-3">
        <Button
          className="w-full justify-start gap-2"
          onClick={handleCreate}
          disabled={createMutation.isPending}
        >
          {createMutation.isPending ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Plus className="h-4 w-4" />
          )}
          New conversation
        </Button>
        <div className="relative mt-2">
          <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            placeholder="Search conversations"
            aria-label="Search conversations"
            className="h-10 pl-9 text-base md:text-sm"
          />
        </div>
      </div>

      <ScrollArea className="flex-1 px-2 pb-2">
        {/* Projects */}
        <div className="mb-1 flex items-center justify-between px-2 pt-1">
          <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            Projects
          </span>
          <button
            className="grid h-9 w-9 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            title="Add project"
            aria-label="Add project"
            onClick={() => setProjectDialogOpen(true)}
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
        </div>

        {projects.length === 0 ? (
          <p className="px-3 pb-2 text-xs text-muted-foreground">
            No projects yet. Add one to keep related conversations together.
          </p>
        ) : (
          <ul className="mb-2 space-y-0.5">
            {visibleProjects.map((p) => {
              const chats = byProject.get(p.id) ?? []
              const visibleChats = chats.slice(0, visibleConversationLimit)
              const hiddenChatCount = chats.length - visibleChats.length
              const isCollapsed = collapsed.has(p.id)
              return (
                <li key={p.id}>
                  <div
                    className={cn(
                      "group flex items-center gap-1 rounded-md px-2 py-1.5 text-sm hover:bg-accent/60",
                      inDrawer && "min-h-11"
                    )}
                  >
                    <button
                      className="flex flex-1 items-center gap-2 overflow-hidden text-left"
                      onClick={() => toggleCollapsed(p.id)}
                      title={p.path ?? p.name}
                    >
                      <ChevronRight
                        className={cn(
                          "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform",
                          !isCollapsed && "rotate-90"
                        )}
                      />
                      {p.type === "local" ? (
                        <FolderOpen className="h-4 w-4 shrink-0 text-muted-foreground" />
                      ) : (
                        <Globe className="h-4 w-4 shrink-0 text-muted-foreground" />
                      )}
                      <span className="truncate font-medium">{p.name}</span>
                      <span className="ml-auto shrink-0 text-[10px] text-muted-foreground/70">
                        {chats.length}
                      </span>
                    </button>
                    <button
                      className="grid h-9 w-9 shrink-0 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:h-7 md:w-7 md:opacity-0 md:transition-opacity md:focus-visible:opacity-100 md:group-hover:opacity-100"
                      title="New conversation in this project"
                      aria-label={`New conversation in ${p.name}`}
                      onClick={() => createInProjectMutation.mutate(p)}
                    >
                      {createInProjectMutation.isPending ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <Plus className="h-3.5 w-3.5" />
                      )}
                    </button>
                    <button
                      className="grid h-9 w-9 shrink-0 place-items-center rounded text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:h-7 md:w-7 md:opacity-0 md:transition-opacity md:focus-visible:opacity-100 md:group-hover:opacity-100"
                      title="Project settings"
                      aria-label={`Settings for ${p.name}`}
                      onClick={() => setSettingsProject(p)}
                    >
                      <Settings2 className="h-3.5 w-3.5" />
                    </button>
                    <button
                      className="grid h-9 w-9 shrink-0 place-items-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:h-7 md:w-7 md:opacity-0 md:focus-visible:opacity-100 md:group-hover:opacity-100"
                      title="Remove project from the sidebar"
                      aria-label={`Remove project ${p.name}`}
                      onClick={() => setDeleteTarget({ kind: "project", project: p })}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                  {!isCollapsed && chats.length > 0 && (
                    <ul className="ml-5 space-y-0.5 border-l pl-2">
                      {visibleChats.map(renderConversationRow)}
                      {hiddenChatCount > 0 && (
                        <li>
                          <Button
                            variant="ghost"
                            className="h-11 w-full justify-start text-xs text-muted-foreground md:h-8"
                            onClick={() =>
                              setVisibleConversationLimit((limit) =>
                                limit + CONVERSATION_BATCH_SIZE
                              )
                            }
                          >
                            Show more ({hiddenChatCount} remaining)
                          </Button>
                        </li>
                      )}
                    </ul>
                  )}
                </li>
              )
            })}
          </ul>
        )}

        {/* Conversations not belonging to any project */}
        <div className="mb-1 px-2 pt-1">
          <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            Conversations
          </span>
        </div>
        {isError ? (
          <QueryErrorState
            title="Conversations could not be loaded"
            description="Check that the local harness is running."
            onRetry={() => void refetch()}
            compact
          />
        ) : isLoading ? (
          <div className="flex items-center justify-center py-8 text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
          </div>
        ) : unassigned.length === 0 ? (
          <p className="px-3 py-4 text-center text-sm text-muted-foreground">
            {searchQuery
              ? "No conversations match your search."
              : conversations.length === 0
                ? "No conversations yet."
                : "No other conversations."}
          </p>
        ) : (
          <ul className="space-y-0.5">
            {unassigned.slice(0, visibleConversationLimit).map(renderConversationRow)}
            {unassigned.length > visibleConversationLimit && (
              <li>
                <Button
                  variant="ghost"
                  className="h-11 w-full justify-start text-xs text-muted-foreground md:h-8"
                  onClick={() =>
                    setVisibleConversationLimit((limit) => limit + CONVERSATION_BATCH_SIZE)
                  }
                >
                  Show more ({unassigned.length - visibleConversationLimit} remaining)
                </Button>
              </li>
            )}
          </ul>
        )}
      </ScrollArea>

      {/* Footer (desktop): section navigation pinned to the bottom. */}
      {!inDrawer && (
        <NavigationGroups
          expanded={expandedNavGroup}
          onExpandedChange={setExpandedNavGroup}
          pathname={location.pathname}
          onNavigate={navigate}
          className="border-t p-2"
        />
      )}

      <ProjectDialog
        open={projectDialogOpen}
        onOpenChange={setProjectDialogOpen}
        onCreated={(convId) => {
          setProjects(loadProjects())
          setConvProjectMap(loadConversationProjectMap())
          queryClient.invalidateQueries({ queryKey: ["conversations"] })
          navigate(`/chat/${convId}`)
        }}
      />

      <ProjectSettingsDialog
        project={settingsProject}
        onOpenChange={(open) => {
          if (!open) setSettingsProject(null)
        }}
        onSaved={(projects) => setProjects(projects)}
      />

      <Dialog open={deleteTarget !== null} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>
              {deleteTarget?.kind === "conversation"
                ? "Delete this conversation?"
                : "Remove this project?"}
            </DialogTitle>
            <DialogDescription>
              {deleteTarget?.kind === "conversation"
                ? `“${deleteTarget.conversation.title || `Conversation #${deleteTarget.conversation.id}`}” and its message history will be permanently deleted.`
                : `“${deleteTarget?.project.name}” will be removed as a grouping. Its conversations will remain available.`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2 sm:space-x-0">
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={deleteMutation.isPending}
              onClick={() => {
                if (deleteTarget?.kind === "conversation") {
                  deleteMutation.mutate(deleteTarget.conversation.id)
                } else if (deleteTarget?.kind === "project") {
                  handleDeleteProject(deleteTarget.project)
                }
              }}
            >
              {deleteMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              {deleteTarget?.kind === "conversation" ? "Delete conversation" : "Remove project"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </aside>
  )
}

function NavigationGroups({
  expanded,
  onExpandedChange,
  pathname,
  onNavigate,
  className,
}: {
  expanded: string | null
  onExpandedChange: (id: string | null) => void
  pathname: string
  onNavigate: (to: string) => void
  className?: string
}) {
  return (
    <nav className={cn("shrink-0 space-y-1", className)} aria-label="Product areas">
      {NAV_GROUPS.map((group) => {
        const open = expanded === group.id
        const active = group.items.some((item) => pathname.startsWith(item.to))
        return (
          <div key={group.id}>
            <button
              type="button"
              className={cn(
                "flex h-9 w-full items-center rounded-md px-2 text-xs font-semibold transition-colors hover:bg-accent",
                active ? "text-foreground" : "text-muted-foreground"
              )}
              aria-expanded={open}
              onClick={() => onExpandedChange(open ? null : group.id)}
            >
              {group.label}
              <ChevronRight
                className={cn("ml-auto h-3.5 w-3.5 transition-transform", open && "rotate-90")}
              />
            </button>
            {open && (
              <div className="grid grid-cols-2 gap-1 py-1">
                {group.items.map(({ to, label, icon: Icon }) => {
                  const itemActive = pathname.startsWith(to)
                  return (
                    <Button
                      key={to}
                      variant="ghost"
                      className={cn(
                        "h-10 min-w-0 justify-start gap-2 px-2 text-xs",
                        itemActive
                          ? "bg-accent text-accent-foreground"
                          : "text-muted-foreground"
                      )}
                      onClick={() => onNavigate(to)}
                    >
                      <Icon className="h-4 w-4" />
                      <span className="truncate">{label}</span>
                    </Button>
                  )
                })}
              </div>
            )}
          </div>
        )
      })}
    </nav>
  )
}
