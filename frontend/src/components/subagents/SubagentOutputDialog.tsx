import { useQuery } from "@tanstack/react-query"
import { Bot, Loader2 } from "lucide-react"
import { subagentsApi } from "@/api/subagents"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Markdown } from "@/components/chat/Markdown"

interface SubagentOutputDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  subagentRunId: number
}

/** Dialog showing the full output of a chat-spawned subagent run. */
export function SubagentOutputDialog({
  open,
  onOpenChange,
  subagentRunId,
}: SubagentOutputDialogProps) {
  const { data: run, isLoading } = useQuery({
    queryKey: ["subagent-run-detail", subagentRunId],
    queryFn: () => subagentsApi.getRun(subagentRunId),
    enabled: open && subagentRunId > 0,
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[80vh] max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Bot className="h-4 w-4" />
            {run?.name ?? "Subagent Output"}
            {run && (
              <span
                className={
                  run.status === "completed"
                    ? "text-xs font-normal text-green-500"
                    : run.status === "failed"
                    ? "text-xs font-normal text-red-500"
                    : "text-xs font-normal text-muted-foreground"
                }
              >
                ({run.status})
              </span>
            )}
          </DialogTitle>
        </DialogHeader>

        {isLoading && (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        )}

        {run && (
          <ScrollArea className="max-h-[60vh]">
            <div className="space-y-3 pr-3">
              {/* Prompt */}
              <div className="rounded-md bg-muted/50 p-3">
                <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                  Prompt
                </p>
                <p className="text-sm">{run.prompt}</p>
              </div>

              {/* Result summary */}
              {run.result_summary && (
                <div className="rounded-md border p-3">
                  <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                    Result
                  </p>
                  <div className="prose prose-sm dark:prose-invert max-w-none">
                    <Markdown content={run.result_summary} />
                  </div>
                </div>
              )}

              {/* Error */}
              {run.error && (
                <div className="rounded-md border border-red-200 bg-red-50 p-3 dark:border-red-900 dark:bg-red-950">
                  <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-red-500">
                    Error
                  </p>
                  <p className="text-sm text-red-600 dark:text-red-400">{run.error}</p>
                </div>
              )}

              {/* Full message history */}
              {run.messages && run.messages.length > 0 && (
                <div className="space-y-2">
                  <p className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                    Conversation ({run.messages.length} messages)
                  </p>
                  {run.messages.map((msg) => (
                    <div
                      key={msg.id}
                      className={
                        msg.role === "assistant"
                          ? "rounded-md border p-2"
                          : msg.role === "tool"
                          ? "rounded-md bg-muted/30 p-2 font-mono text-xs"
                          : "rounded-md bg-muted/50 p-2"
                      }
                    >
                      <span className="mb-0.5 block text-[10px] font-semibold uppercase text-muted-foreground">
                        {msg.role}
                      </span>
                      {msg.content && msg.role === "assistant" ? (
                        <div className="prose prose-sm dark:prose-invert max-w-none">
                          <Markdown content={msg.content} />
                        </div>
                      ) : (
                        <p className="whitespace-pre-wrap text-xs">{msg.content ?? "(empty)"}</p>
                      )}
                    </div>
                  ))}
                </div>
              )}
            </div>
          </ScrollArea>
        )}
      </DialogContent>
    </Dialog>
  )
}
