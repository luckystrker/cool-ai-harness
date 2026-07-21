import { useState } from "react"
import { useNavigate, useParams } from "react-router-dom"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Plus, Settings, Trash2, MessageSquare, Loader2 } from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { cn } from "@/lib/utils"

export function Sidebar() {
  const navigate = useNavigate()
  const { conversationId } = useParams()
  const queryClient = useQueryClient()
  const [newOpen, setNewOpen] = useState(false)
  const [title, setTitle] = useState("")

  const { data: conversations = [], isLoading } = useQuery({
    queryKey: ["conversations"],
    queryFn: conversationsApi.list,
  })

  const createMutation = useMutation({
    mutationFn: conversationsApi.create,
    onSuccess: (conv) => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
      setNewOpen(false)
      setTitle("")
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

  const handleCreate = () => createMutation.mutate({ title: title.trim() || undefined })
  const handleDelete = (id: number) => deleteMutation.mutate(id)

  return (
    <aside className="flex w-72 shrink-0 flex-col border-r bg-muted/30">
      {/* Brand */}
      <div className="flex h-14 items-center gap-2 border-b px-4">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <span className="text-sm font-bold">H</span>
        </div>
        <span className="font-semibold tracking-tight">Harness</span>
      </div>

      {/* New chat */}
      <div className="p-3">
        <Dialog open={newOpen} onOpenChange={setNewOpen}>
          <DialogTrigger asChild>
            <Button className="w-full justify-start gap-2">
              <Plus className="h-4 w-4" /> New conversation
            </Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>New conversation</DialogTitle>
            </DialogHeader>
            <Input
              autoFocus
              placeholder="Title (optional)"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleCreate()
              }}
            />
            <DialogFooter>
              <Button variant="outline" onClick={() => setNewOpen(false)}>
                Cancel
              </Button>
              <Button onClick={handleCreate} disabled={createMutation.isPending}>
                {createMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                Create
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </div>

      {/* Conversation list */}
      <ScrollArea className="flex-1 px-2 pb-2">
        {isLoading ? (
          <div className="flex items-center justify-center py-8 text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
          </div>
        ) : conversations.length === 0 ? (
          <p className="px-3 py-8 text-center text-sm text-muted-foreground">
            No conversations yet.
          </p>
        ) : (
          <ul className="space-y-0.5">
            {conversations.map((c) => {
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
                      <span className="truncate">
                        {c.title || `Conversation #${c.id}`}
                      </span>
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
            })}
          </ul>
        )}
      </ScrollArea>

      {/* Footer */}
      <div className="border-t p-2">
        <Button
          asChild
          variant="ghost"
          className="w-full justify-start gap-2"
        >
          <a href="/settings">
            <Settings className="h-4 w-4" /> Settings
          </a>
        </Button>
      </div>
    </aside>
  )
}
