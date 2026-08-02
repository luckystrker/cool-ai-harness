import { useMemo, useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  AlertCircle,
  CalendarClock,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Globe,
  Inbox,
  Loader2,
  Pause,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Rss,
  SkipForward,
  Trash2,
  Webhook,
  XCircle,
} from "lucide-react"
import { toast } from "sonner"
import { rssApi } from "@/api/rss"
import { tasksApi } from "@/api/tasks"
import { webhooksApi } from "@/api/webhooks"
import type {
  RssEntry,
  RssSubscription,
  ScheduledTask,
  ScheduledTaskCreate,
  ScheduledTaskUpdate,
  TaskDeliveryChannel,
  TaskRun,
  TaskRunStatus,
  WebhookEndpoint,
  WebhookEvent,
} from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"

type Tab = "tasks" | "inbox" | "rss" | "webhooks"

function formatWhen(iso: string | null | undefined): string {
  if (!iso) return "—"
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return "—"
  return date.toLocaleString()
}

function formatDuration(ms: number | null): string {
  if (ms == null) return "—"
  if (ms < 1000) return `${ms}ms`
  return `${(ms / 1000).toFixed(1)}s`
}

const STATUS_ICONS: Record<TaskRunStatus, typeof CheckCircle2> = {
  queued: Loader2,
  running: Loader2,
  completed: CheckCircle2,
  failed: XCircle,
  cancelled: XCircle,
  skipped: SkipForward,
}

function StatusBadge({ status }: { status: TaskRunStatus | null }) {
  if (!status) return <Badge variant="outline">never run</Badge>
  const Icon = STATUS_ICONS[status] ?? AlertCircle
  const spinning = status === "running" || status === "queued"
  const variant =
    status === "completed" ? "secondary" : status === "failed" ? "destructive" : "outline"
  return (
    <Badge variant={variant} className="gap-1">
      <Icon className={cn("h-3 w-3", spinning && "animate-spin")} /> {status}
    </Badge>
  )
}

export function TasksPage() {
  const queryClient = useQueryClient()
  const [tab, setTab] = useState<Tab>("tasks")
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<ScheduledTask | null>(null)
  const [expanded, setExpanded] = useState<number | null>(null)

  const { data: tasks = [], isLoading } = useQuery({
    queryKey: ["tasks"],
    queryFn: () => tasksApi.list(),
    refetchInterval: 10_000,
  })

  const { data: scheduler } = useQuery({
    queryKey: ["tasks", "scheduler"],
    queryFn: tasksApi.scheduler,
  })

  const { data: inbox } = useQuery({
    queryKey: ["tasks", "inbox"],
    queryFn: () => tasksApi.inbox({ limit: 50 }),
    refetchInterval: 10_000,
  })

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["tasks"] })

  const createMutation = useMutation({
    mutationFn: (body: ScheduledTaskCreate) => tasksApi.create(body),
    onSuccess: () => {
      invalidate()
      setDialogOpen(false)
      toast.success("Task created")
    },
    onError: (e) => toast.error("Failed to create task", { description: String(e) }),
  })

  const updateMutation = useMutation({
    mutationFn: ({ id, body }: { id: number; body: ScheduledTaskUpdate }) =>
      tasksApi.update(id, body),
    onSuccess: () => {
      invalidate()
      setDialogOpen(false)
      setEditing(null)
      toast.success("Task updated")
    },
    onError: (e) => toast.error("Failed to update task", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => tasksApi.delete(id),
    onSuccess: () => {
      invalidate()
      toast.success("Task deleted")
    },
    onError: (e) => toast.error("Failed to delete task", { description: String(e) }),
  })

  const runMutation = useMutation({
    mutationFn: (id: number) => tasksApi.runNow(id),
    onSuccess: () => {
      invalidate()
      toast.success("Run started", { description: "Result appears in the inbox." })
    },
    onError: (e) => toast.error("Failed to start run", { description: String(e) }),
  })

  const toggleMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      tasksApi.update(id, { enabled }),
    onSuccess: () => invalidate(),
  })

  if (isLoading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  return (
    <div className="mx-auto max-w-5xl space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <CalendarClock className="h-6 w-6" />
          <h1 className="text-2xl font-bold">Scheduled Tasks</h1>
          {scheduler && (
            <Badge variant={scheduler.running ? "secondary" : "outline"} className="ml-1">
              scheduler {scheduler.running ? "running" : scheduler.enabled ? "idle" : "off"}
            </Badge>
          )}
        </div>
        <Button
          onClick={() => {
            setEditing(null)
            setDialogOpen(true)
          }}
        >
          <Plus className="mr-1 h-4 w-4" /> New Task
        </Button>
      </div>

      <div className="flex gap-1 border-b">
        {(["tasks", "inbox", "rss", "webhooks"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className={cn(
              "flex items-center gap-1.5 px-4 py-2 text-sm font-medium capitalize transition-colors",
              tab === t ? "border-b-2 border-primary text-foreground" : "text-muted-foreground"
            )}
          >
            {t === "inbox" && <Inbox className="h-3.5 w-3.5" />}
            {t === "rss" && <Rss className="h-3.5 w-3.5" />}
            {t === "webhooks" && <Webhook className="h-3.5 w-3.5" />}
            {t}
            {t === "inbox" && (inbox?.unread_count ?? 0) > 0 && (
              <span className="ml-1 rounded-full bg-primary px-1.5 text-[10px] text-primary-foreground">
                {inbox?.unread_count}
              </span>
            )}
          </button>
        ))}
      </div>

      {tab === "tasks" && (
        <div className="space-y-3">
          {tasks.length === 0 && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              No scheduled tasks yet. Create one, or ask the agent in chat:
              "every Monday at 9am send me a news digest".
            </p>
          )}
          {tasks.map((task) => (
            <TaskCard
              key={task.id}
              task={task}
              expanded={expanded === task.id}
              onToggleExpand={() => setExpanded(expanded === task.id ? null : task.id)}
              onEdit={() => {
                setEditing(task)
                setDialogOpen(true)
              }}
              onDelete={() => deleteMutation.mutate(task.id)}
              onRun={() => runMutation.mutate(task.id)}
              onToggleEnabled={() =>
                toggleMutation.mutate({ id: task.id, enabled: !task.enabled })
              }
            />
          ))}
        </div>
      )}

      {tab === "inbox" && <InboxPanel />}

      {tab === "rss" && <RssPanel />}

      {tab === "webhooks" && <WebhooksPanel />}

      <TaskDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        task={editing}
        onCreate={(body) => createMutation.mutate(body)}
        onUpdate={(id, body) => updateMutation.mutate({ id, body })}
      />
    </div>
  )
}

// --- Task card with expandable run history ---

function TaskCard({
  task,
  expanded,
  onToggleExpand,
  onEdit,
  onDelete,
  onRun,
  onToggleEnabled,
}: {
  task: ScheduledTask
  expanded: boolean
  onToggleExpand: () => void
  onEdit: () => void
  onDelete: () => void
  onRun: () => void
  onToggleEnabled: () => void
}) {
  const { data: runs = [] } = useQuery({
    queryKey: ["tasks", "runs", task.id],
    queryFn: () => tasksApi.listRuns(task.id, { limit: 20 }),
    enabled: expanded,
  })

  return (
    <Card className={cn(!task.enabled && "opacity-60")}>
      <CardHeader className="pb-2">
        <div className="flex items-center gap-2">
          <button onClick={onToggleExpand} className="shrink-0">
            {expanded ? (
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-4 w-4 text-muted-foreground" />
            )}
          </button>
          <CardTitle className="text-base">{task.name}</CardTitle>
          <StatusBadge status={task.last_status} />
          {!task.enabled && (
            <Badge variant="outline" className="text-xs">
              paused
            </Badge>
          )}
          {task.workflow_type && (
            <Badge variant="secondary" className="text-xs">
              {task.workflow_type}
            </Badge>
          )}
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
          <span title={task.cron_expression ?? ""}>
            {task.schedule_description ?? task.cron_expression ?? task.trigger_type}
          </span>
          <span>Next: {formatWhen(task.next_run_at)}</span>
          <span>Runs: {task.run_count}</span>
          {task.failure_count > 0 && (
            <span className="text-destructive">Failures: {task.failure_count}</span>
          )}
        </div>
      </CardHeader>
      <CardContent className="flex items-center gap-1 pt-0">
        <Button variant="ghost" size="sm" onClick={onRun} title="Run now">
          <Play className="h-3.5 w-3.5" />
        </Button>
        <Button variant="ghost" size="sm" onClick={onToggleEnabled} title="Enable/Pause">
          {task.enabled ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
        </Button>
        <Button variant="ghost" size="sm" onClick={onEdit} title="Edit">
          <Pencil className="h-3.5 w-3.5" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          className="text-destructive"
          onClick={onDelete}
          title="Delete"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
        {task.next_runs.length > 0 && (
          <span className="ml-auto text-[11px] text-muted-foreground">
            Upcoming: {task.next_runs.map((r) => formatWhen(r)).join(", ")}
          </span>
        )}
      </CardContent>

      {expanded && (
        <div className="border-t px-4 py-2">
          <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
            Run history
          </span>
          {runs.length === 0 ? (
            <p className="py-2 text-xs text-muted-foreground">No runs yet.</p>
          ) : (
            <ul className="mt-1 space-y-1">
              {runs.map((run) => (
                <li key={run.id} className="flex items-center gap-2 text-xs">
                  <StatusBadge status={run.status} />
                  <span className="text-muted-foreground">{formatWhen(run.started_at)}</span>
                  <span className="text-muted-foreground">{formatDuration(run.duration_ms)}</span>
                  <span className="text-muted-foreground">{run.trigger_source}</span>
                  {run.error && (
                    <span className="truncate text-destructive" title={run.error}>
                      {run.error}
                    </span>
                  )}
                  {run.output && !run.error && (
                    <span className="truncate text-muted-foreground" title={run.output}>
                      {run.output.slice(0, 80)}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Card>
  )
}

// --- Inbox panel ---

function InboxPanel() {
  const queryClient = useQueryClient()
  const { data: inbox } = useQuery({
    queryKey: ["tasks", "inbox"],
    queryFn: () => tasksApi.inbox({ limit: 50 }),
    refetchInterval: 10_000,
  })

  const markReadMutation = useMutation({
    mutationFn: (runId: number) => tasksApi.markRead(runId, true),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["tasks"] }),
  })

  const runs: TaskRun[] = inbox?.runs ?? []

  if (runs.length === 0) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        Inbox is empty. Task results will appear here.
      </p>
    )
  }

  return (
    <div className="space-y-2">
      {runs.map((run) => (
        <div
          key={run.id}
          className={cn(
            "flex items-start gap-3 rounded-md border p-3 text-sm",
            !run.is_read && "border-primary/40 bg-primary/5"
          )}
        >
          <StatusBadge status={run.status} />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <span>#{run.task_id}</span>
              <span>{run.trigger_source}</span>
              <span>{formatWhen(run.started_at)}</span>
              <span>{formatDuration(run.duration_ms)}</span>
            </div>
            {run.output && (
              <p className="mt-1 line-clamp-2 whitespace-pre-wrap text-xs">{run.output}</p>
            )}
            {run.error && (
              <p className="mt-1 text-xs text-destructive">{run.error}</p>
            )}
            {run.skip_reason && (
              <p className="mt-1 text-xs text-muted-foreground">Skipped: {run.skip_reason}</p>
            )}
          </div>
          {!run.is_read && (
            <Button
              variant="ghost"
              size="sm"
              className="shrink-0 text-xs"
              onClick={() => markReadMutation.mutate(run.id)}
            >
              Mark read
            </Button>
          )}
        </div>
      ))}
    </div>
  )
}

// --- Create / Edit dialog ---

function TaskDialog({
  open,
  onOpenChange,
  task,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (v: boolean) => void
  task: ScheduledTask | null
  onCreate: (body: ScheduledTaskCreate) => void
  onUpdate: (id: number, body: ScheduledTaskUpdate) => void
}) {
  const [name, setName] = useState("")
  const [prompt, setPrompt] = useState("")
  const [schedule, setSchedule] = useState("")
  const [timezone, setTimezone] = useState("")
  const [model, setModel] = useState("")
  const [maxIterations, setMaxIterations] = useState("10")
  const [channels, setChannels] = useState<TaskDeliveryChannel[]>(["ui"])
  const [webhookUrl, setWebhookUrl] = useState("")
  const [approvalPolicy, setApprovalPolicy] = useState<"deny_external" | "allow_all">(
    "deny_external"
  )
  const [preview, setPreview] = useState<{ description: string | null; next_runs: string[] } | null>(
    null
  )

  // Sync form when dialog opens.
  const [lastTask, setLastTask] = useState<ScheduledTask | null>(null)
  if (open && task !== lastTask) {
    setLastTask(task)
    setName(task?.name ?? "")
    setPrompt(task?.prompt ?? "")
    setSchedule(task?.cron_expression ?? "")
    setTimezone(task?.timezone ?? "")
    setModel(task?.model ?? "")
    setMaxIterations(String(task?.max_iterations ?? 10))
    setChannels((task?.delivery_channels as TaskDeliveryChannel[]) ?? ["ui"])
    setWebhookUrl(
      (task?.delivery_config as Record<string, string> | null)?.webhook_url ?? ""
    )
    setApprovalPolicy(task?.approval_policy ?? "deny_external")
    setPreview(
      task?.schedule_description
        ? { description: task.schedule_description, next_runs: task.next_runs }
        : null
    )
  }
  if (!open && lastTask !== null) {
    setLastTask(null)
  }

  const parsedSchedule = useMemo(() => {
    if (!schedule.trim()) return null
    return preview
  }, [schedule, preview])

  const handleScheduleBlur = async () => {
    if (!schedule.trim()) {
      setPreview(null)
      return
    }
    try {
      const resp = await tasksApi.parseCron(schedule.trim())
      if (resp.cron_expression) {
        setSchedule(resp.cron_expression)
        setPreview({ description: resp.description, next_runs: resp.next_runs })
      } else {
        setPreview(null)
        toast.error("Could not parse schedule", { description: resp.detail ?? undefined })
      }
    } catch {
      setPreview(null)
    }
  }

  const handleSubmit = () => {
    const body: ScheduledTaskCreate = {
      name: name.trim(),
      prompt: prompt.trim() || undefined,
      cron_expression: schedule.trim() || undefined,
      timezone: timezone.trim() || undefined,
      model: model.trim() || undefined,
      max_iterations: Number(maxIterations) || 10,
      delivery_channels: channels,
      delivery_config:
        channels.includes("webhook") && webhookUrl.trim()
          ? { webhook_url: webhookUrl.trim() }
          : undefined,
      approval_policy: approvalPolicy,
    }
    if (task) {
      onUpdate(task.id, body)
    } else {
      onCreate(body)
    }
  }

  const toggleChannel = (ch: TaskDeliveryChannel) => {
    setChannels((prev) =>
      prev.includes(ch) ? prev.filter((c) => c !== ch) : [...prev, ch]
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] max-w-lg overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{task ? "Edit Task" : "New Scheduled Task"}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 pt-2">
          <div className="space-y-1">
            <Label>Name</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Daily news digest" />
          </div>

          <div className="space-y-1">
            <Label>Prompt</Label>
            <Textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              rows={4}
              placeholder="What should the agent do on every run?"
            />
          </div>

          <div className="space-y-1">
            <Label>Schedule (cron or natural language)</Label>
            <Input
              value={schedule}
              onChange={(e) => setSchedule(e.target.value)}
              onBlur={handleScheduleBlur}
              placeholder="0 9 * * 1  or  every monday at 9am"
              className="font-mono text-sm"
            />
            {parsedSchedule && (
              <p className="text-xs text-muted-foreground">
                {parsedSchedule.description}
                {parsedSchedule.next_runs.length > 0 && (
                  <> — next: {parsedSchedule.next_runs.map(formatWhen).join(", ")}</>
                )}
              </p>
            )}
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1">
              <Label>Timezone</Label>
              <Input
                value={timezone}
                onChange={(e) => setTimezone(e.target.value)}
                placeholder="UTC"
              />
            </div>
            <div className="space-y-1">
              <Label>Model (optional)</Label>
              <Input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gpt-4o"
              />
            </div>
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1">
              <Label>Max iterations</Label>
              <Input
                type="number"
                value={maxIterations}
                onChange={(e) => setMaxIterations(e.target.value)}
                min={1}
                max={50}
              />
            </div>
            <div className="space-y-1">
              <Label>External side effects</Label>
              <select
                value={approvalPolicy}
                onChange={(e) =>
                  setApprovalPolicy(e.target.value as "deny_external" | "allow_all")
                }
                className="h-9 w-full rounded-md border bg-background px-3 text-sm"
              >
                <option value="deny_external">Deny (safe default)</option>
                <option value="allow_all">Allow (pre-approved)</option>
              </select>
            </div>
          </div>

          <div className="space-y-1">
            <Label>Delivery channels</Label>
            <div className="flex gap-3">
              {(["ui", "webhook"] as TaskDeliveryChannel[]).map((ch) => (
                <label key={ch} className="flex items-center gap-1.5 text-sm">
                  <input
                    type="checkbox"
                    checked={channels.includes(ch)}
                    onChange={() => toggleChannel(ch)}
                  />
                  {ch}
                </label>
              ))}
            </div>
            {channels.includes("webhook") && (
              <Input
                value={webhookUrl}
                onChange={(e) => setWebhookUrl(e.target.value)}
                placeholder="https://example.com/hook"
                className="mt-1"
              />
            )}
          </div>

          <div className="flex justify-end gap-2 pt-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={handleSubmit} disabled={!name.trim() || !schedule.trim()}>
              {task ? "Save" : "Create"}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

// --- RSS Panel ---

function RssPanel() {
  const queryClient = useQueryClient()
  const [url, setUrl] = useState("")
  const [category, setCategory] = useState("")

  const { data: subscriptions = [] } = useQuery({
    queryKey: ["rss", "subscriptions"],
    queryFn: () => rssApi.listSubscriptions(),
    refetchInterval: 30_000,
  })

  const { data: entries = [] } = useQuery({
    queryKey: ["rss", "entries"],
    queryFn: () => rssApi.allEntries({ limit: 30 }),
    refetchInterval: 30_000,
  })

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["rss"] })
  }

  const subscribeMutation = useMutation({
    mutationFn: () => rssApi.subscribe({ url, category: category || undefined }),
    onSuccess: () => {
      invalidate()
      setUrl("")
      setCategory("")
      toast.success("Subscribed")
    },
    onError: (e) => toast.error("Failed to subscribe", { description: String(e) }),
  })

  const unsubscribeMutation = useMutation({
    mutationFn: (id: number) => rssApi.unsubscribe(id),
    onSuccess: () => {
      invalidate()
      toast.success("Unsubscribed")
    },
  })

  const fetchMutation = useMutation({
    mutationFn: (id: number) => rssApi.fetchNow(id),
    onSuccess: (data) => {
      invalidate()
      toast.success(`Fetched ${data.new_entries} new entries`)
    },
  })

  const markReadMutation = useMutation({
    mutationFn: (id: number) => rssApi.markRead(id),
    onSuccess: () => invalidate(),
  })

  return (
    <div className="space-y-4">
      {/* Add subscription */}
      <div className="flex gap-2">
        <Input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://example.com/feed.xml"
          className="flex-1"
        />
        <Input
          value={category}
          onChange={(e) => setCategory(e.target.value)}
          placeholder="category"
          className="w-32"
        />
        <Button onClick={() => subscribeMutation.mutate()} disabled={!url.trim()}>
          <Plus className="mr-1 h-4 w-4" /> Subscribe
        </Button>
      </div>

      {/* Subscriptions list */}
      <div className="space-y-2">
        <h3 className="text-sm font-medium text-muted-foreground">
          Subscriptions ({subscriptions.length})
        </h3>
        {subscriptions.map((sub: RssSubscription) => (
          <Card key={sub.id}>
            <CardContent className="flex items-center justify-between py-3">
              <div className="min-w-0">
                <p className="truncate text-sm font-medium">{sub.title || sub.url}</p>
                <p className="truncate text-xs text-muted-foreground">{sub.url}</p>
                <div className="mt-1 flex gap-2">
                  {sub.category && <Badge variant="outline">{sub.category}</Badge>}
                  <Badge variant="secondary">{sub.entry_count} entries</Badge>
                  {sub.last_error && (
                    <Badge variant="destructive" className="max-w-40 truncate">
                      {sub.last_error}
                    </Badge>
                  )}
                </div>
              </div>
              <div className="flex gap-1">
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => fetchMutation.mutate(sub.id)}
                  title="Fetch now"
                >
                  <RefreshCw className="h-4 w-4" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => unsubscribeMutation.mutate(sub.id)}
                  title="Unsubscribe"
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {/* Recent entries */}
      <div className="space-y-2">
        <h3 className="text-sm font-medium text-muted-foreground">Recent entries</h3>
        {entries.length === 0 && (
          <p className="py-4 text-center text-sm text-muted-foreground">No entries yet.</p>
        )}
        {entries.map((entry: RssEntry) => (
          <div
            key={entry.id}
            className={cn(
              "flex items-start gap-2 rounded-md border p-2",
              !entry.is_read && "bg-accent/50"
            )}
          >
            <Globe className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
            <div className="min-w-0 flex-1">
              {entry.link ? (
                <a
                  href={entry.link}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm font-medium hover:underline"
                >
                  {entry.title || "(no title)"}
                </a>
              ) : (
                <span className="text-sm font-medium">{entry.title || "(no title)"}</span>
              )}
              <p className="text-xs text-muted-foreground">
                {entry.author && `${entry.author} · `}
                {formatWhen(entry.published_at)}
              </p>
            </div>
            {!entry.is_read && (
              <Button
                variant="ghost"
                size="icon"
                className="h-6 w-6"
                onClick={() => markReadMutation.mutate(entry.id)}
                title="Mark read"
              >
                <CheckCircle2 className="h-3.5 w-3.5" />
              </Button>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}

// --- Webhooks Panel ---

function WebhooksPanel() {
  const queryClient = useQueryClient()
  const [name, setName] = useState("")
  const [sourceType, setSourceType] = useState("custom")
  const [expandedEp, setExpandedEp] = useState<number | null>(null)

  const { data: endpoints = [] } = useQuery({
    queryKey: ["webhooks"],
    queryFn: () => webhooksApi.list(),
    refetchInterval: 15_000,
  })

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["webhooks"] })

  const createMutation = useMutation({
    mutationFn: () => webhooksApi.create({ name, source_type: sourceType }),
    onSuccess: () => {
      invalidate()
      setName("")
      toast.success("Webhook created")
    },
    onError: (e) => toast.error("Failed to create webhook", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => webhooksApi.delete(id),
    onSuccess: () => {
      invalidate()
      toast.success("Webhook deleted")
    },
  })

  const replayMutation = useMutation({
    mutationFn: ({ epId, evId }: { epId: number; evId: number }) =>
      webhooksApi.replay(epId, evId),
    onSuccess: () => {
      invalidate()
      toast.success("Event replayed")
    },
  })

  return (
    <div className="space-y-4">
      {/* Create endpoint */}
      <div className="flex gap-2">
        <Input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Webhook name"
          className="flex-1"
        />
        <select
          value={sourceType}
          onChange={(e) => setSourceType(e.target.value)}
          className="rounded-md border bg-background px-3 py-1.5 text-sm"
        >
          <option value="custom">Custom</option>
          <option value="github">GitHub</option>
          <option value="notion">Notion</option>
          <option value="slack">Slack</option>
        </select>
        <Button onClick={() => createMutation.mutate()} disabled={!name.trim()}>
          <Plus className="mr-1 h-4 w-4" /> Create
        </Button>
      </div>

      {/* Endpoints list */}
      {endpoints.length === 0 && (
        <p className="py-8 text-center text-sm text-muted-foreground">
          No webhook endpoints yet. Create one to receive events from external systems.
        </p>
      )}
      {endpoints.map((ep: WebhookEndpoint) => (
        <EndpointCard
          key={ep.id}
          endpoint={ep}
          expanded={expandedEp === ep.id}
          onToggle={() => setExpandedEp(expandedEp === ep.id ? null : ep.id)}
          onDelete={() => deleteMutation.mutate(ep.id)}
          onReplay={(evId) => replayMutation.mutate({ epId: ep.id, evId })}
        />
      ))}
    </div>
  )
}

function EndpointCard({
  endpoint,
  expanded,
  onToggle,
  onDelete,
  onReplay,
}: {
  endpoint: WebhookEndpoint
  expanded: boolean
  onToggle: () => void
  onDelete: () => void
  onReplay: (eventId: number) => void
}) {
  const { data: events = [] } = useQuery({
    queryKey: ["webhooks", endpoint.id, "events"],
    queryFn: () => webhooksApi.listEvents(endpoint.id, { limit: 20 }),
    enabled: expanded,
  })

  return (
    <Card>
      <CardHeader className="cursor-pointer py-3" onClick={onToggle}>
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            {expanded ? (
              <ChevronDown className="h-4 w-4" />
            ) : (
              <ChevronRight className="h-4 w-4" />
            )}
            <CardTitle className="text-sm">{endpoint.name}</CardTitle>
            <Badge variant="outline">{endpoint.source_type}</Badge>
            {!endpoint.enabled && <Badge variant="secondary">disabled</Badge>}
          </div>
          <Button variant="ghost" size="icon" onClick={(e) => { e.stopPropagation(); onDelete() }}>
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </CardHeader>
      {expanded && (
        <CardContent className="space-y-3 pt-0">
          <div className="rounded-md bg-muted p-2 text-xs font-mono">
            <p className="text-muted-foreground">URL</p>
            <p className="select-all">{endpoint.url_path}</p>
            <p className="mt-1 text-muted-foreground">Secret</p>
            <p className="select-all">{endpoint.secret}</p>
          </div>
          <div className="space-y-1">
            <h4 className="text-xs font-medium text-muted-foreground">
              Events ({events.length})
            </h4>
            {events.map((ev: WebhookEvent) => (
              <div
                key={ev.id}
                className="flex items-center justify-between rounded border px-2 py-1 text-xs"
              >
                <div className="flex items-center gap-2">
                  <Badge
                    variant={
                      ev.status === "completed"
                        ? "secondary"
                        : ev.status === "rejected" || ev.status === "failed"
                          ? "destructive"
                          : "outline"
                    }
                  >
                    {ev.status}
                  </Badge>
                  <span>{ev.event_type || "unknown"}</span>
                  <span className="text-muted-foreground">{formatWhen(ev.received_at)}</span>
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-5 w-5"
                  onClick={() => onReplay(ev.id)}
                  title="Replay"
                >
                  <RefreshCw className="h-3 w-3" />
                </Button>
              </div>
            ))}
          </div>
        </CardContent>
      )}
    </Card>
  )
}
