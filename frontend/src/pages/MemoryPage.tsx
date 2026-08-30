import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  Brain,
  Box,
  ChevronDown,
  ChevronRight,
  Clock,
  Database,
  Download,
  Globe,
  Loader2,
  Pencil,
  Pin,
  PinOff,
  Plus,
  Save,
  Trash2,
  X,
} from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { memoryApi } from "@/api/memory"
import type { MemoryItem, MemoryScope, MemoryType } from "@/api/types"
import { EntitiesPanel } from "@/components/memory/EntitiesPanel"
import { ExplainPanel } from "@/components/memory/ExplainPanel"
import { ReviewQueue } from "@/components/memory/ReviewQueue"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Textarea } from "@/components/ui/textarea"
import { QueryErrorState } from "@/components/ui/query-state"
import { cn } from "@/lib/utils"

type TabKey = "global" | "agent" | "review" | "entities"

const TYPE_COLORS: Record<MemoryType, string> = {
  semantic: "bg-blue-500/15 text-blue-600 dark:text-blue-400",
  episodic: "bg-purple-500/15 text-purple-600 dark:text-purple-400",
  procedural: "bg-green-500/15 text-green-600 dark:text-green-400",
  preference: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
}

const SCOPE_LABELS: Record<MemoryScope, string> = {
  global: "System",
  agent: "Agent",
  conversation: "Session",
}

export function MemoryPage() {
  const queryClient = useQueryClient()
  const [activeTab, setActiveTab] = useState<TabKey>("global")
  const [editingMemory, setEditingMemory] = useState<MemoryItem | null>(null)
  const [createOpen, setCreateOpen] = useState(false)

  // Fetch memories by scope
  const {
    data: globalMemories = [],
    isLoading: globalLoading,
    isError: globalError,
    refetch: refetchGlobal,
  } = useQuery({
    queryKey: ["memories", "global"],
    queryFn: () => memoryApi.list({ scope: "global", limit: 100 }),
  })

  const {
    data: agentMemories = [],
    isLoading: agentLoading,
    isError: agentError,
    refetch: refetchAgent,
  } = useQuery({
    queryKey: ["memories", "agent"],
    queryFn: () => memoryApi.list({ scope: "agent", limit: 100 }),
  })

  const { data: stats } = useQuery({
    queryKey: ["memory-stats"],
    queryFn: memoryApi.stats,
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => memoryApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["memories"] })
      queryClient.invalidateQueries({ queryKey: ["memory-stats"] })
      toast.success("Memory archived")
    },
    onError: (error) =>
      toast.error("Memory was not archived", {
        description: getErrorDescription(error, "Refresh the memory list and try again."),
      }),
  })

  const handleExport = (format: "json" | "markdown") => {
    memoryApi.exportMemories(format).catch((e) =>
      toast.error("Memories were not exported", {
        description: getErrorDescription(e, "Try the export again."),
      })
    )
  }

  const scopeLoading = activeTab === "global" ? globalLoading : agentLoading
  const scopeMemories = activeTab === "global" ? globalMemories : agentMemories
  const scopeError = activeTab === "global" ? globalError : agentError
  const refetchScope = activeTab === "global" ? refetchGlobal : refetchAgent

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex flex-col gap-3 border-b px-4 py-4 sm:flex-row sm:items-center sm:justify-between sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <Brain className="h-6 w-6 text-primary" />
          <div className="min-w-0">
            <h1 className="text-lg font-semibold">Memory</h1>
            <p className="text-sm leading-5 text-muted-foreground">
              Facts and preferences the agent can reuse in future conversations.
            </p>
          </div>
        </div>
        <div className="grid grid-cols-2 gap-2 sm:flex sm:items-center">
          <ExportMenu onExport={handleExport} />
          {activeTab !== "entities" && activeTab !== "review" && (
            <Button onClick={() => setCreateOpen(true)} className="gap-2">
              <Plus className="h-4 w-4" />
              Add memory
            </Button>
          )}
        </div>
      </div>

      {/* Stats bar */}
      {stats && (
        <div className="flex flex-wrap gap-x-4 gap-y-2 border-b px-4 py-3 text-sm sm:px-6">
          <span className="text-muted-foreground">
            Active: <strong className="text-foreground">{stats.total_active}</strong>
          </span>
          <span className="text-muted-foreground">
            Episodes: <strong className="text-foreground">{stats.total_episodes}</strong>
          </span>
          <span className="text-muted-foreground">
            Entities: <strong className="text-foreground">{stats.total_entities}</strong>
          </span>
          {stats.total_pending > 0 && (
            <span className="text-amber-600 dark:text-amber-400">
              Pending review: <strong>{stats.total_pending}</strong>
            </span>
          )}
          <span className="text-muted-foreground">
            Archived: <strong className="text-foreground">{stats.total_archived}</strong>
          </span>
          {Object.entries(stats.by_type).map(([type, count]) => (
            <Badge key={type} variant="secondary" className="capitalize">
              {type}: {count}
            </Badge>
          ))}
        </div>
      )}

      {/* Tabs */}
      <div className="flex min-h-14 gap-1 overflow-x-auto border-b px-4 py-2 sm:px-6">
        <TabButton
          active={activeTab === "global"}
          onClick={() => setActiveTab("global")}
          icon={<Globe className="h-4 w-4" />}
          label="All projects"
          count={globalMemories.length}
        />
        <TabButton
          active={activeTab === "agent"}
          onClick={() => setActiveTab("agent")}
          icon={<Database className="h-4 w-4" />}
          label="Current project"
          count={agentMemories.length}
        />
        <TabButton
          active={activeTab === "review"}
          onClick={() => setActiveTab("review")}
          icon={<Clock className="h-4 w-4" />}
          label="Review"
          count={stats?.total_pending ?? 0}
          highlight={(stats?.total_pending ?? 0) > 0}
        />
        <TabButton
          active={activeTab === "entities"}
          onClick={() => setActiveTab("entities")}
          icon={<Box className="h-4 w-4" />}
          label="Entities"
          count={stats?.total_entities ?? 0}
        />
      </div>

      {/* Tab content */}
      {activeTab === "review" ? (
        <ReviewQueue />
      ) : activeTab === "entities" ? (
        <EntitiesPanel />
      ) : (
        <MemoryList
          memories={scopeMemories}
          isLoading={scopeLoading}
          isError={scopeError}
          onRetry={() => void refetchScope()}
          onEdit={(m) => setEditingMemory(m)}
          onDelete={(id) => deleteMutation.mutate(id)}
        />
      )}

      {/* Edit dialog */}
      <MemoryEditDialog
        memory={editingMemory}
        onClose={() => setEditingMemory(null)}
      />

      {/* Create dialog */}
      <MemoryCreateDialog
        open={createOpen}
        defaultScope={activeTab === "agent" ? "agent" : "global"}
        onClose={() => setCreateOpen(false)}
      />
    </div>
  )
}

function ExportMenu({ onExport }: { onExport: (format: "json" | "markdown") => void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" className="gap-2">
          <Download className="h-4 w-4" />
          Export
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-40">
        <DropdownMenuItem onSelect={() => onExport("json")}>JSON</DropdownMenuItem>
        <DropdownMenuItem onSelect={() => onExport("markdown")}>
          Markdown
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function TabButton({
  active,
  onClick,
  icon,
  label,
  count,
  highlight,
}: {
  active: boolean
  onClick: () => void
  icon: React.ReactNode
  label: string
  count: number
  highlight?: boolean
}) {
  return (
    <button
      className={cn(
        "flex min-h-11 shrink-0 items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
        active
          ? "bg-accent text-accent-foreground"
          : "text-muted-foreground hover:bg-accent/50"
      )}
      onClick={onClick}
    >
      {icon}
      {label}
      <span
        className={cn(
          "rounded-full px-1.5 py-0.5 text-xs",
          highlight
            ? "bg-amber-500/20 text-amber-700 dark:text-amber-300"
            : "bg-muted"
        )}
      >
        {count}
      </span>
    </button>
  )
}

function MemoryList({
  memories,
  isLoading,
  isError,
  onRetry,
  onEdit,
  onDelete,
}: {
  memories: MemoryItem[]
  isLoading: boolean
  isError: boolean
  onRetry: () => void
  onEdit: (m: MemoryItem) => void
  onDelete: (id: number) => void
}) {
  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12 text-muted-foreground">
        <Loader2 className="h-5 w-5 animate-spin" />
      </div>
    )
  }
  if (isError) {
    return (
      <QueryErrorState
        title="Memories could not be loaded"
        description="Check that Cool is running locally, then try again."
        onRetry={onRetry}
      />
    )
  }
  if (memories.length === 0) {
    return (
      <div className="py-12 text-center text-muted-foreground">
        <Brain className="mx-auto mb-3 h-10 w-10 opacity-30" />
        <p>No memories here yet.</p>
        <p className="text-sm">
          The agent can create memories during conversations, or you can add one manually.
        </p>
      </div>
    )
  }
  return (
    <ScrollArea className="flex-1">
      <div className="space-y-2 p-6">
        {memories.map((memory) => (
          <MemoryCard
            key={memory.id}
            memory={memory}
            onEdit={() => onEdit(memory)}
            onDelete={() => onDelete(memory.id)}
          />
        ))}
      </div>
    </ScrollArea>
  )
}

function MemoryCard({
  memory,
  onEdit,
  onDelete,
}: {
  memory: MemoryItem
  onEdit: () => void
  onDelete: () => void
}) {
  const queryClient = useQueryClient()
  const [expanded, setExpanded] = useState(false)

  const pinMutation = useMutation({
    mutationFn: (pinned: boolean) => memoryApi.pin(memory.id, pinned),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["memories"] })
      toast.success(memory.pinned ? "Unpinned" : "Pinned")
    },
    onError: (error) =>
      toast.error("Memory pin was not changed", {
        description: getErrorDescription(error, "Refresh the memory list and try again."),
      }),
  })

  return (
    <Card className="group">
      <CardContent className="flex items-start gap-4 p-4">
        <button
          className="mt-0.5 text-muted-foreground hover:text-foreground"
          onClick={() => setExpanded((e) => !e)}
          aria-label={expanded ? "Collapse" : "Expand why remembered"}
        >
          {expanded ? (
            <ChevronDown className="h-4 w-4" />
          ) : (
            <ChevronRight className="h-4 w-4" />
          )}
        </button>
        <div className="flex-1 space-y-2">
          <div className="flex items-center gap-2">
            <Badge className={cn("capitalize", TYPE_COLORS[memory.memory_type])}>
              {memory.memory_type}
            </Badge>
            <Badge variant="outline" className="text-xs">
              {SCOPE_LABELS[memory.scope]}
            </Badge>
            {memory.pinned && (
              <Badge variant="outline" className="gap-1 text-xs">
                <Pin className="h-3 w-3" /> pinned
              </Badge>
            )}
            {memory.tags?.map((tag) => (
              <Badge key={tag} variant="secondary" className="text-xs">
                {tag}
              </Badge>
            ))}
          </div>
          <p className="text-sm leading-relaxed">{memory.content}</p>
          <div className="flex items-center gap-4 text-xs text-muted-foreground">
            <span>Importance: {(memory.importance * 100).toFixed(0)}%</span>
            <span>Confidence: {(memory.confidence * 100).toFixed(0)}%</span>
            <span>Accessed: {memory.access_count}x</span>
            <span>Source: {memory.source}</span>
          </div>
          {expanded && <ExplainPanel memoryId={memory.id} />}
        </div>
        <div className="flex gap-1 opacity-100 transition-opacity md:opacity-0 md:group-focus-within:opacity-100 md:group-hover:opacity-100">
          <Button
            size="icon"
            variant="ghost"
            className="h-8 w-8"
            onClick={() => pinMutation.mutate(!memory.pinned)}
            title={memory.pinned ? "Unpin" : "Pin (protect from decay)"}
            aria-label={memory.pinned ? "Unpin memory" : "Pin memory"}
          >
            {memory.pinned ? (
              <PinOff className="h-4 w-4" />
            ) : (
              <Pin className="h-4 w-4" />
            )}
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className="h-8 w-8"
            onClick={onEdit}
            aria-label="Edit memory"
          >
            <Pencil className="h-4 w-4" />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className="h-8 w-8 text-destructive hover:text-destructive"
            onClick={onDelete}
            title="Archive"
            aria-label="Archive memory"
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

function MemoryEditDialog({
  memory,
  onClose,
}: {
  memory: MemoryItem | null
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [content, setContent] = useState("")
  const [importance, setImportance] = useState(0.5)
  const [tags, setTags] = useState("")

  // Sync state when memory changes
  const [lastMemoryId, setLastMemoryId] = useState<number | null>(null)
  if (memory && memory.id !== lastMemoryId) {
    setLastMemoryId(memory.id)
    setContent(memory.content)
    setImportance(memory.importance)
    setTags(memory.tags?.join(", ") ?? "")
  }

  const updateMutation = useMutation({
    mutationFn: (body: { content: string; importance: number; tags: string[] }) =>
      memoryApi.update(memory!.id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["memories"] })
      toast.success("Memory updated")
      onClose()
    },
    onError: (error) =>
      toast.error("Memory changes were not saved", {
        description: getErrorDescription(error, "Review the memory fields and try again."),
      }),
  })

  const handleSave = () => {
    if (!memory) return
    updateMutation.mutate({
      content,
      importance,
      tags: tags
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
    })
  }

  return (
    <Dialog open={!!memory} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Edit memory</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-4">
          <div className="space-y-2">
            <Label htmlFor="memory-edit-content">Content</Label>
            <Textarea
              id="memory-edit-content"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              rows={4}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="memory-edit-importance">
              Importance: {(importance * 100).toFixed(0)}%
            </Label>
            <input
              id="memory-edit-importance"
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={importance}
              onChange={(e) => setImportance(Number(e.target.value))}
              className="w-full"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="memory-edit-tags">Tags (comma-separated)</Label>
            <Input
              id="memory-edit-tags"
              value={tags}
              onChange={(e) => setTags(e.target.value)}
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={handleSave} disabled={updateMutation.isPending}>
            {updateMutation.isPending ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <Save className="mr-2 h-4 w-4" />
            )}
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function MemoryCreateDialog({
  open,
  defaultScope,
  onClose,
}: {
  open: boolean
  defaultScope: "global" | "agent"
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [content, setContent] = useState("")
  const [memoryType, setMemoryType] = useState<MemoryType>("semantic")
  const [scope, setScope] = useState<MemoryScope>(defaultScope)
  const [importance, setImportance] = useState(0.5)
  const [tags, setTags] = useState("")

  const createMutation = useMutation({
    mutationFn: memoryApi.create,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["memories"] })
      queryClient.invalidateQueries({ queryKey: ["memory-stats"] })
      toast.success("Memory created")
      onClose()
      setContent("")
      setTags("")
    },
    onError: (error) =>
      toast.error("Memory was not created", {
        description: getErrorDescription(error, "Review the memory fields and try again."),
      }),
  })

  const handleCreate = () => {
    if (!content.trim()) return
    createMutation.mutate({
      content: content.trim(),
      memory_type: memoryType,
      scope,
      importance,
      tags: tags
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean),
    })
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Add memory</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-4">
          <div className="space-y-2">
            <Label htmlFor="memory-create-content">Content</Label>
            <Textarea
              id="memory-create-content"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              placeholder="What should the agent remember?"
              rows={3}
            />
          </div>
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="memory-create-type">Type</Label>
              <select
                id="memory-create-type"
                value={memoryType}
                onChange={(e) => setMemoryType(e.target.value as MemoryType)}
                className="w-full rounded-md border bg-background px-3 py-2 text-sm"
              >
                <option value="semantic">Semantic (fact)</option>
                <option value="procedural">Procedural (how-to)</option>
                <option value="preference">Preference</option>
                <option value="episodic">Episodic (event)</option>
              </select>
            </div>
            <div className="space-y-2">
              <Label htmlFor="memory-create-scope">Scope</Label>
              <select
                id="memory-create-scope"
                value={scope}
                onChange={(e) => setScope(e.target.value as MemoryScope)}
                className="w-full rounded-md border bg-background px-3 py-2 text-sm"
              >
                <option value="global">Global (all agents)</option>
                <option value="agent">Agent (project-specific)</option>
              </select>
            </div>
          </div>
          <div className="space-y-2">
            <Label htmlFor="memory-create-importance">
              Importance: {(importance * 100).toFixed(0)}%
            </Label>
            <input
              id="memory-create-importance"
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={importance}
              onChange={(e) => setImportance(Number(e.target.value))}
              className="w-full"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="memory-create-tags">Tags (comma-separated)</Label>
            <Input
              id="memory-create-tags"
              value={tags}
              onChange={(e) => setTags(e.target.value)}
              placeholder="python, testing, deployment"
            />
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            <X className="mr-2 h-4 w-4" />
            Cancel
          </Button>
          <Button
            onClick={handleCreate}
            disabled={!content.trim() || createMutation.isPending}
          >
            {createMutation.isPending ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <Plus className="mr-2 h-4 w-4" />
            )}
            Add memory
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
