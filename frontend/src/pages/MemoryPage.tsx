import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  Brain,
  Database,
  Globe,
  Loader2,
  Pencil,
  Plus,
  Save,
  Trash2,
  X,
} from "lucide-react"
import { toast } from "sonner"
import { memoryApi } from "@/api/memory"
import type { MemoryItem, MemoryScope, MemoryType } from "@/api/types"
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
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"

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
  const [activeTab, setActiveTab] = useState<"global" | "agent">("global")
  const [editingMemory, setEditingMemory] = useState<MemoryItem | null>(null)
  const [createOpen, setCreateOpen] = useState(false)

  // Fetch memories by scope
  const { data: globalMemories = [], isLoading: globalLoading } = useQuery({
    queryKey: ["memories", "global"],
    queryFn: () => memoryApi.list({ scope: "global", limit: 100 }),
  })

  const { data: agentMemories = [], isLoading: agentLoading } = useQuery({
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
    onError: (e) => toast.error("Failed to delete", { description: String(e) }),
  })

  const memories = activeTab === "global" ? globalMemories : agentMemories
  const isLoading = activeTab === "global" ? globalLoading : agentLoading

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b px-6 py-4">
        <div className="flex items-center gap-3">
          <Brain className="h-6 w-6 text-primary" />
          <div>
            <h1 className="text-lg font-semibold">Memory</h1>
            <p className="text-sm text-muted-foreground">
              Long-term memory across sessions
            </p>
          </div>
        </div>
        <Button onClick={() => setCreateOpen(true)} className="gap-2">
          <Plus className="h-4 w-4" />
          Add memory
        </Button>
      </div>

      {/* Stats bar */}
      {stats && (
        <div className="flex gap-4 border-b px-6 py-3 text-sm">
          <span className="text-muted-foreground">
            Active: <strong className="text-foreground">{stats.total_active}</strong>
          </span>
          <span className="text-muted-foreground">
            Episodes: <strong className="text-foreground">{stats.total_episodes}</strong>
          </span>
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
      <div className="flex gap-1 border-b px-6 py-2">
        <TabButton
          active={activeTab === "global"}
          onClick={() => setActiveTab("global")}
          icon={<Globe className="h-4 w-4" />}
          label="System (Global)"
          count={globalMemories.length}
        />
        <TabButton
          active={activeTab === "agent"}
          onClick={() => setActiveTab("agent")}
          icon={<Database className="h-4 w-4" />}
          label="Agent (Project)"
          count={agentMemories.length}
        />
      </div>

      {/* Memory list */}
      <ScrollArea className="flex-1">
        <div className="space-y-2 p-6">
          {isLoading ? (
            <div className="flex items-center justify-center py-12 text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin" />
            </div>
          ) : memories.length === 0 ? (
            <div className="py-12 text-center text-muted-foreground">
              <Brain className="mx-auto mb-3 h-10 w-10 opacity-30" />
              <p>No memories in this scope yet.</p>
              <p className="text-sm">
                Memories are created automatically during conversations or manually.
              </p>
            </div>
          ) : (
            memories.map((memory) => (
              <MemoryCard
                key={memory.id}
                memory={memory}
                onEdit={() => setEditingMemory(memory)}
                onDelete={() => deleteMutation.mutate(memory.id)}
              />
            ))
          )}
        </div>
      </ScrollArea>

      {/* Edit dialog */}
      <MemoryEditDialog
        memory={editingMemory}
        onClose={() => setEditingMemory(null)}
      />

      {/* Create dialog */}
      <MemoryCreateDialog
        open={createOpen}
        defaultScope={activeTab}
        onClose={() => setCreateOpen(false)}
      />
    </div>
  )
}

function TabButton({
  active,
  onClick,
  icon,
  label,
  count,
}: {
  active: boolean
  onClick: () => void
  icon: React.ReactNode
  label: string
  count: number
}) {
  return (
    <button
      className={cn(
        "flex items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
        active
          ? "bg-accent text-accent-foreground"
          : "text-muted-foreground hover:bg-accent/50"
      )}
      onClick={onClick}
    >
      {icon}
      {label}
      <span className="rounded-full bg-muted px-1.5 py-0.5 text-xs">{count}</span>
    </button>
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
  return (
    <Card className="group">
      <CardContent className="flex items-start gap-4 p-4">
        <div className="flex-1 space-y-2">
          <div className="flex items-center gap-2">
            <Badge className={cn("capitalize", TYPE_COLORS[memory.memory_type])}>
              {memory.memory_type}
            </Badge>
            <Badge variant="outline" className="text-xs">
              {SCOPE_LABELS[memory.scope]}
            </Badge>
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
        </div>
        <div className="flex gap-1 opacity-0 transition-opacity group-hover:opacity-100">
          <Button size="icon" variant="ghost" className="h-8 w-8" onClick={onEdit}>
            <Pencil className="h-4 w-4" />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className="h-8 w-8 text-destructive hover:text-destructive"
            onClick={onDelete}
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
    onError: (e) => toast.error("Failed to update", { description: String(e) }),
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
          <DialogTitle>Edit Memory</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-4">
          <div className="space-y-2">
            <Label>Content</Label>
            <Textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              rows={4}
            />
          </div>
          <div className="space-y-2">
            <Label>Importance: {(importance * 100).toFixed(0)}%</Label>
            <input
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
            <Label>Tags (comma-separated)</Label>
            <Input value={tags} onChange={(e) => setTags(e.target.value)} />
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
    onError: (e) => toast.error("Failed to create", { description: String(e) }),
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
          <DialogTitle>Add Memory</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-4">
          <div className="space-y-2">
            <Label>Content</Label>
            <Textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              placeholder="What should the agent remember?"
              rows={3}
            />
          </div>
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label>Type</Label>
              <select
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
              <Label>Scope</Label>
              <select
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
            <Label>Importance: {(importance * 100).toFixed(0)}%</Label>
            <input
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
            <Label>Tags (comma-separated)</Label>
            <Input
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
            Create
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
