import { useMemo, useState } from "react"
import { useNavigate, useParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  BarChart3,
  BookOpen,
  Bot,
  Brain,
  Bug,
  ChevronRight,
  FolderOpen,
  Globe,
  Loader2,
  MessageSquare,
  Plus,
  Settings,
  Settings2,
  Trash2,
  Wallet,
} from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import type { Conversation } from "@/api/types"
import { loadAgentDefaults, loadLastModel } from "@/lib/agentConfig"
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
import { ScrollArea } from "@/components/ui/scroll-area"
import { cn } from "@/lib/utils"

export function Sidebar() {
  const navigate = useNavigate()
  const { conversationId } = useParams()
  const queryClient = useQueryClient()

  const { data: conversations = [], isLoading } = useQuery({
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

  const createMutation = useMutation({
    mutationFn: conversationsApi.create,
    onSuccess: (conv) => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      navigate(`/chat/${conv.id}`)
    },
    onError: (e) => toast.error("Failed to create conversation", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: conversationsApi.delete,
    onSuccess: (_data, deletedId) => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      if (Number(conversationId) === deletedId) navigate("/")
    },
    onError: (e) => toast.error("Failed to delete", { description: String(e) }),
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
    onError: (e) => toast.error("Failed to create chat", { description: String(e) }),
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
  const handleDelete = (id: number) => deleteMutation.mutate(id)

  const handleDeleteProject = (id: string) => {
    setProjects(deleteProject(id))
    setConvProjectMap(loadConversationProjectMap())
  }

  const toggleCollapsed = (id: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  // Split conversations into per-project buckets + an unassigned list.
  const { byProject, unassigned } = useMemo(() => {
    const map = new Map<string, Conversation[]>()
    const rest: Conversation[] = []
    for (const c of conversations) {
      const pid = convProjectMap[String(c.id)]
      if (pid && projects.some((p) => p.id === pid)) {
        if (!map.has(pid)) map.set(pid, [])
        map.get(pid)!.push(c)
      } else {
        rest.push(c)
      }
    }
    return { byProject: map, unassigned: rest }
  }, [conversations, convProjectMap, projects])

  const renderConversationRow = (c: Conversation) => {
    const active = Number(conversationId) === c.id
    return (
      <li key={c.id}>
        <div
          className={cn(
            "group flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors",
            active ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
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
            className="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:text-destructive"
            title="Delete"
            onClick={() => handleDelete(c.id)}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </div>
      </li>
    )
  }

  return (
    <aside className="flex w-72 shrink-0 flex-col border-r bg-muted/30">
      {/* Brand */}
      <div className="flex h-14 items-center gap-2 border-b px-4">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <span className="text-sm font-bold">H</span>
        </div>
        <span className="font-semibold tracking-tight">Harness</span>
      </div>

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
      </div>

      <ScrollArea className="flex-1 px-2 pb-2">
        {/* Projects */}
        <div className="mb-1 flex items-center justify-between px-2 pt-1">
          <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            Projects
          </span>
          <button
            className="rounded p-0.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            title="Add project"
            onClick={() => setProjectDialogOpen(true)}
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
        </div>

        {projects.length === 0 ? (
          <p className="px-3 pb-2 text-xs text-muted-foreground">
            No projects yet. Add one to group chats under a folder.
          </p>
        ) : (
          <ul className="mb-2 space-y-0.5">
            {projects.map((p) => {
              const chats = byProject.get(p.id) ?? []
              const isCollapsed = collapsed.has(p.id)
              return (
                <li key={p.id}>
                  <div className="group flex items-center gap-1 rounded-md px-2 py-1.5 text-sm hover:bg-accent/60">
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
                      className="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:text-foreground"
                      title="New chat in this project"
                      onClick={() => createInProjectMutation.mutate(p)}
                    >
                      {createInProjectMutation.isPending ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <Plus className="h-3.5 w-3.5" />
                      )}
                    </button>
                    <button
                      className="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:text-foreground"
                      title="Project settings"
                      onClick={() => setSettingsProject(p)}
                    >
                      <Settings2 className="h-3.5 w-3.5" />
                    </button>
                    <button
                      className="opacity-0 transition-opacity group-hover:opacity-100 text-muted-foreground hover:text-destructive"
                      title="Delete project"
                      onClick={() => handleDeleteProject(p.id)}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                  {!isCollapsed && chats.length > 0 && (
                    <ul className="ml-5 space-y-0.5 border-l pl-2">
                      {chats.map(renderConversationRow)}
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
        {isLoading ? (
          <div className="flex items-center justify-center py-8 text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
          </div>
        ) : unassigned.length === 0 ? (
          <p className="px-3 py-4 text-center text-sm text-muted-foreground">
            {conversations.length === 0 ? "No conversations yet." : "No other conversations."}
          </p>
        ) : (
          <ul className="space-y-0.5">{unassigned.map(renderConversationRow)}</ul>
        )}
      </ScrollArea>

      {/* Footer */}
      <div className="space-y-1 border-t p-2">
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/memory">
            <Brain className="h-4 w-4" /> Memory
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/wiki">
            <BookOpen className="h-4 w-4" /> Wiki
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/profiles">
            <Settings2 className="h-4 w-4" /> Profiles
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/analytics">
            <BarChart3 className="h-4 w-4" /> Analytics
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/subagents">
            <Bot className="h-4 w-4" /> Subagents
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/budgets">
            <Wallet className="h-4 w-4" /> Budgets
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/inspector">
            <Bug className="h-4 w-4" /> Inspector
          </a>
        </Button>
        <Button asChild variant="ghost" className="w-full justify-start gap-2">
          <a href="/settings">
            <Settings className="h-4 w-4" /> Settings
          </a>
        </Button>
      </div>

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
    </aside>
  )
}
