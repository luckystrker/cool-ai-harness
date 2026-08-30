import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Check, Clock, Loader2, X } from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { memoryApi } from "@/api/memory"
import type { MemoryItem } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import { ScrollArea } from "@/components/ui/scroll-area"
import { cn } from "@/lib/utils"

const TYPE_COLORS: Record<string, string> = {
  semantic: "bg-blue-500/15 text-blue-600 dark:text-blue-400",
  episodic: "bg-purple-500/15 text-purple-600 dark:text-purple-400",
  procedural: "bg-green-500/15 text-green-600 dark:text-green-400",
  preference: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
}

/** Pending-confirmation queue: agent-extracted memories awaiting user review. */
export function ReviewQueue() {
  const queryClient = useQueryClient()
  const { data: pending = [], isLoading } = useQuery({
    queryKey: ["memories", "pending"],
    queryFn: () => memoryApi.listPending(),
  })

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["memories"] })
    queryClient.invalidateQueries({ queryKey: ["memory-stats"] })
  }

  const confirmMutation = useMutation({
    mutationFn: (id: number) => memoryApi.confirm(id),
    onSuccess: () => {
      invalidate()
      toast.success("Memory confirmed")
    },
    onError: (error) =>
      toast.error("Memory was not confirmed", {
        description: getErrorDescription(error, "Refresh the review queue and try again."),
      }),
  })

  const rejectMutation = useMutation({
    mutationFn: (id: number) => memoryApi.reject(id),
    onSuccess: () => {
      invalidate()
      toast.success("Memory rejected")
    },
    onError: (error) =>
      toast.error("Memory was not rejected", {
        description: getErrorDescription(error, "Refresh the review queue and try again."),
      }),
  })

  return (
    <ScrollArea className="flex-1">
      <div className="space-y-2 p-6">
        {isLoading ? (
          <div className="flex items-center justify-center py-12 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
          </div>
        ) : pending.length === 0 ? (
          <div className="py-12 text-center text-muted-foreground">
            <Clock className="mx-auto mb-3 h-10 w-10 opacity-30" />
            <p>No memories awaiting review.</p>
            <p className="text-sm">
              Agent-extracted memories will appear here for your confirmation.
            </p>
          </div>
        ) : (
          pending.map((memory) => (
            <PendingCard
              key={memory.id}
              memory={memory}
              onConfirm={() => confirmMutation.mutate(memory.id)}
              onReject={() => rejectMutation.mutate(memory.id)}
              pending={
                confirmMutation.isPending || rejectMutation.isPending
              }
            />
          ))
        )}
      </div>
    </ScrollArea>
  )
}

function PendingCard({
  memory,
  onConfirm,
  onReject,
  pending,
}: {
  memory: MemoryItem
  onConfirm: () => void
  onReject: () => void
  pending: boolean
}) {
  return (
    <Card>
      <CardContent className="flex items-start gap-4 p-4">
        <div className="flex-1 space-y-2">
          <div className="flex items-center gap-2">
            <Badge className={cn("capitalize", TYPE_COLORS[memory.memory_type])}>
              {memory.memory_type}
            </Badge>
            <Badge variant="outline" className="text-xs">
              {Math.round(memory.confidence * 100)}% confident
            </Badge>
            {memory.tags?.map((tag) => (
              <Badge key={tag} variant="secondary" className="text-xs">
                {tag}
              </Badge>
            ))}
          </div>
          <p className="text-sm leading-relaxed">{memory.content}</p>
          <div className="text-xs text-muted-foreground">
            Source: {memory.source} · added {new Date(memory.created_at).toLocaleDateString()}
          </div>
        </div>
        <div className="flex gap-1">
          <Button
            size="sm"
            variant="default"
            className="gap-1"
            onClick={onConfirm}
            disabled={pending}
          >
            <Check className="h-4 w-4" />
            Approve
          </Button>
          <Button
            size="sm"
            variant="ghost"
            className="gap-1 text-destructive hover:text-destructive"
            onClick={onReject}
            disabled={pending}
          >
            <X className="h-4 w-4" />
            Reject
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
