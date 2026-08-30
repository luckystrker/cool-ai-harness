import { useMemo, useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { KeyRound, Plus, Trash2, Loader2, CheckCircle2, ChevronRight, Pencil, ShieldCheck, FileText, RotateCcw, Star, Sparkles, Plug, Unplug, RefreshCw, Server, Search, Download, Store } from "lucide-react"
import { toast } from "sonner"
import { providersApi } from "@/api/providers"
import { settingsApi } from "@/api/settings"
import { skillsApi } from "@/api/skills"
import { mcpApi } from "@/api/mcp"
import type {
  BreakpointConfig,
  BreakpointType,
  CapabilityPolicy,
  MCPServer,
  MCPServerCreate,
  MCPStoreItem,
  Provider,
  ProviderCreate,
  ProviderUpdate,
  SkillInfo,
  ToolPermission,
  ToolPermissions,
} from "@/api/types"
import {
  BREAKPOINT_TYPES,
  CAPABILITY_NAMES,
  MODE_LABELS,
  MODE_PRESETS,
  PERMISSIONS,
  PERM_STYLES,
  TOOL_NAMES,
  loadAgentDefaults,
  saveAgentDefaults,
} from "@/lib/agentConfig"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { ChatModelsPicker } from "@/components/settings/ChatModelsPicker"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import { Textarea } from "@/components/ui/textarea"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { cn } from "@/lib/utils"

const EMPTY_FORM: ProviderCreate = {
  name: "openai",
  label: "",
  base_url: "https://api.openai.com/v1",
  api_key: "",
  default_model: undefined,
  is_subscription: false,
  is_fallback: false,
  chat_models: [],
}

export function SettingsPage() {
  const queryClient = useQueryClient()
  const [activeSection, setActiveSection] = useState<
    "connections" | "agent" | "extensions" | "prompt"
  >("connections")
  const [createOpen, setCreateOpen] = useState(false)
  const [createForm, setCreateForm] = useState<ProviderCreate>(EMPTY_FORM)
  const [editing, setEditing] = useState<Provider | null>(null)
  const [deletingProvider, setDeletingProvider] = useState<Provider | null>(null)

  const { data: providers = [], isLoading } = useQuery({
    queryKey: ["providers"],
    queryFn: providersApi.list,
  })

  const createMutation = useMutation({
    mutationFn: providersApi.create,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider added")
      setCreateOpen(false)
      setCreateForm(EMPTY_FORM)
    },
    onError: () => toast.error("Provider could not be added", {
      description: "Check the endpoint and API key, then try again.",
    }),
  })

  const updateMutation = useMutation({
    mutationFn: (vars: { id: number; body: ProviderUpdate }) =>
      providersApi.update(vars.id, vars.body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider updated")
      setEditing(null)
    },
    onError: () => toast.error("Provider changes were not saved", {
      description: "Check the endpoint and credentials, then try again.",
    }),
  })

  const deleteMutation = useMutation({
    mutationFn: providersApi.delete,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider deleted")
      setDeletingProvider(null)
    },
    onError: () => toast.error("Provider was not deleted", {
      description: "It may still be in use. Refresh the page and try again.",
    }),
  })

  const handleCreate = () => {
    if (!createForm.api_key.trim()) {
      toast.error("API key is required")
      return
    }
    createMutation.mutate(createForm)
  }

  return (
    <div className="h-full overflow-x-hidden overflow-y-auto">
      <div className="mx-auto max-w-3xl space-y-6 p-4 sm:p-6">
        <header>
          <h1 className="sr-only text-2xl font-semibold tracking-[-0.02em] md:not-sr-only">Settings</h1>
          <p className="mt-1 max-w-[65ch] text-sm text-muted-foreground">
            Configure model connections, agent safety, extensions, and the default prompt.
          </p>
        </header>

        <div className="grid grid-cols-2 gap-1 rounded-lg bg-muted p-1 sm:grid-cols-4" role="tablist" aria-label="Settings sections">
          {([
            ["connections", "Connections", KeyRound],
            ["agent", "Agent", ShieldCheck],
            ["extensions", "Extensions", Plug],
            ["prompt", "Prompt", FileText],
          ] as const).map(([id, label, Icon]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={activeSection === id}
              onClick={() => setActiveSection(id)}
              className={cn(
                "flex min-h-10 items-center justify-center gap-2 rounded-md px-3 text-sm font-medium transition-colors",
                activeSection === id
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground"
              )}
            >
              <Icon className="h-4 w-4" />
              {label}
            </button>
          ))}
        </div>

        {activeSection === "connections" && (
          <section className="space-y-4" aria-labelledby="providers-heading">
            <div className="flex flex-col items-start gap-4 sm:flex-row sm:items-center sm:justify-between">
              <div>
                <h2 id="providers-heading" className="text-lg font-semibold">Model providers</h2>
                <p className="text-sm text-muted-foreground">
                  API keys are encrypted at rest and never shown in full.
                </p>
              </div>

              <Dialog open={createOpen} onOpenChange={setCreateOpen}>
                <DialogTrigger asChild>
                  <Button className="gap-2">
                    <Plus className="h-4 w-4" /> Add provider
                  </Button>
                </DialogTrigger>
                <DialogContent>
                  <DialogHeader>
                    <DialogTitle>Add provider</DialogTitle>
                  </DialogHeader>
                  <ProviderForm form={createForm} onChange={setCreateForm} />
                  <DialogFooter>
                    <Button variant="outline" onClick={() => setCreateOpen(false)}>
                      Cancel
                    </Button>
                    <Button onClick={handleCreate} disabled={createMutation.isPending}>
                      {createMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                      Save provider
                    </Button>
                  </DialogFooter>
                </DialogContent>
              </Dialog>
            </div>

            {isLoading ? (
              <div className="flex justify-center py-12">
                <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
              </div>
            ) : providers.length === 0 ? (
              <div className="rounded-xl border border-dashed px-5 py-10 text-center">
                <h3 className="font-medium">Connect a model provider</h3>
                <p className="mx-auto mt-1 max-w-md text-sm text-muted-foreground">
                  Add an OpenAI-compatible or Anthropic endpoint before starting a model run.
                </p>
                <Button className="mt-4" onClick={() => setCreateOpen(true)}>
                  <Plus className="h-4 w-4" /> Add your first provider
                </Button>
              </div>
            ) : (
              <div className="space-y-3">
                {providers.map((p) => (
                  <ProviderRow
                    key={p.id}
                    provider={p}
                    onEdit={() => setEditing(p)}
                    onDelete={() => setDeletingProvider(p)}
                    deleting={deleteMutation.isPending}
                    onSetDefault={() =>
                      updateMutation.mutate({ id: p.id, body: { is_default: true } })
                    }
                    settingDefault={
                      updateMutation.isPending &&
                      updateMutation.variables?.id === p.id
                    }
                  />
                ))}
              </div>
            )}
          </section>
        )}

        {activeSection === "agent" && <AgentConfigSection />}
        {activeSection === "extensions" && (
          <div className="space-y-6">
            <SkillsSection />
            <MCPServersSection />
            <MCPStoreSection />
          </div>
        )}
        {activeSection === "prompt" && <SystemPromptSection />}
      </div>

      <EditProviderDialog
        // Remount the dialog for each provider so its internal form state
        // reseeds from the new provider automatically.
        key={editing?.id ?? "none"}
        provider={editing}
        onClose={() => setEditing(null)}
        onSubmit={(body) => {
          if (!editing) return
          updateMutation.mutate({ id: editing.id, body })
        }}
        pending={updateMutation.isPending}
      />

      <Dialog open={deletingProvider !== null} onOpenChange={(open) => !open && setDeletingProvider(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Delete this provider?</DialogTitle>
            <DialogDescription>
              {deletingProvider
                ? `“${deletingProvider.label || deletingProvider.name}” and its stored credentials will be permanently removed.`
                : "This provider will be permanently removed."}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="gap-2 sm:space-x-0">
            <Button variant="outline" onClick={() => setDeletingProvider(null)}>Cancel</Button>
            <Button
              variant="destructive"
              disabled={deleteMutation.isPending}
              onClick={() => deletingProvider && deleteMutation.mutate(deletingProvider.id)}
            >
              {deleteMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              Delete provider
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

function ProviderRow({
  provider: p,
  onEdit,
  onDelete,
  deleting,
  onSetDefault,
  settingDefault,
}: {
  provider: Provider
  onEdit: () => void
  onDelete: () => void
  deleting: boolean
  onSetDefault: () => void
  settingDefault: boolean
}) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <CardTitle className="text-base">{p.label || p.name}</CardTitle>
            <Badge variant="outline" className="font-mono">{p.name}</Badge>
            {p.is_subscription && <Badge variant="secondary">subscription</Badge>}
            {p.is_fallback && <Badge variant="outline">fallback</Badge>}
            {p.is_default && (
              <Badge variant="default" className="gap-1">
                <Star className="h-3 w-3" /> default
              </Badge>
            )}
            {p.is_active ? (
              <Badge variant="success" className="gap-1">
                <CheckCircle2 className="h-3 w-3" /> active
              </Badge>
            ) : (
              <Badge variant="warning">disabled</Badge>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-2 sm:justify-end">
            <label
              className="flex cursor-pointer items-center gap-1.5 text-xs text-muted-foreground"
              title="Use as the default provider for new conversations"
            >
              <input
                type="radio"
                name="default-provider"
                checked={p.is_default}
                onChange={onSetDefault}
                disabled={settingDefault || !p.is_active}
                className="h-3.5 w-3.5"
              />
              {settingDefault ? (
                <Loader2 className="h-3 w-3 animate-spin" />
              ) : (
                <span>Default</span>
              )}
            </label>
            <Button
              variant="ghost"
              size="icon"
              className="text-muted-foreground hover:text-foreground"
              onClick={onEdit}
              title={`Edit ${p.label || p.name}`}
              aria-label={`Edit ${p.label || p.name}`}
            >
              <Pencil className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="text-muted-foreground hover:text-destructive"
              onClick={onDelete}
              disabled={deleting}
              title={`Delete ${p.label || p.name}`}
              aria-label={`Delete ${p.label || p.name}`}
            >
              {deleting ? <Loader2 className="h-4 w-4 animate-spin" /> : <Trash2 className="h-4 w-4" />}
            </Button>
          </div>
        </div>
        <CardDescription>
          {p.base_url || "(default endpoint)"} ·{" "}
          {p.chat_models.length
            ? `${p.chat_models.length} chat model${p.chat_models.length === 1 ? "" : "s"}`
            : "no chat models"}
        </CardDescription>
      </CardHeader>
      <CardContent className="pt-0">
        {p.api_key_hint ? (
          <div className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
            <KeyRound className="h-3.5 w-3.5" />
            <span>{p.api_key_hint}</span>
          </div>
        ) : (
          <span className="text-xs text-muted-foreground">No key set</span>
        )}
      </CardContent>
    </Card>
  )
}

/** Create-provider form. */
function ProviderForm({
  form,
  onChange,
}: {
  form: ProviderCreate
  onChange: (next: ProviderCreate) => void
}) {
  const set = (patch: Partial<ProviderCreate>) => onChange({ ...form, ...patch })

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-3">
        <div className="space-y-1.5">
          <Label htmlFor="p-name">Provider</Label>
          <Input
            id="p-name"
            placeholder="openai"
            value={form.name}
            onChange={(e) => set({ name: e.target.value })}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="p-label">Label</Label>
          <Input
            id="p-label"
            placeholder="Personal"
            value={form.label ?? ""}
            onChange={(e) => set({ label: e.target.value })}
          />
        </div>
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="p-base">Base URL</Label>
        <Input
          id="p-base"
          placeholder="https://api.openai.com/v1"
          value={form.base_url ?? ""}
          onChange={(e) => set({ base_url: e.target.value })}
        />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="p-key">API key</Label>
        <Textarea
          id="p-key"
          placeholder="sk-…"
          value={form.api_key}
          onChange={(e) => set({ api_key: e.target.value })}
          className="font-mono text-xs"
          rows={2}
        />
      </div>
      <ChatModelsPicker
        id="p-chat-models"
        mode="preview"
        value={form.chat_models ?? []}
        onChange={(models) => set({ chat_models: models })}
        previewRequest={{
          name: form.name,
          base_url: form.base_url ?? undefined,
          api_key: form.api_key,
        }}
      />
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={!!form.is_fallback}
          onChange={(e) => set({ is_fallback: e.target.checked })}
          className="h-4 w-4 rounded border-input"
        />
        <span>
          Use as <strong>fallback</strong> provider
          <span className="block text-xs text-muted-foreground">
            Activated when the primary provider is unhealthy (retry/circuit-breaker).
          </span>
        </span>
      </label>
    </div>
  )
}

/**
 * Edit-provider dialog. The provider identifier (`name`) is read-only — it's
 * an opaque key the backend routes by, not something users should rename.
 *
 * Mounting this with `key={provider.id}` (done by the caller) makes the
 * internal `useState` initializers re-run for each provider, so the form
 * reseeds automatically without a render-phase effect.
 */
function EditProviderDialog({
  provider,
  onClose,
  onSubmit,
  pending,
}: {
  provider: Provider | null
  onClose: () => void
  onSubmit: (body: ProviderUpdate) => void
  pending: boolean
}) {
  const [label, setLabel] = useState(provider?.label ?? "")
  const [base_url, setBaseUrl] = useState(provider?.base_url ?? "")
  const [chat_models, setChatModels] = useState<string[]>(provider?.chat_models ?? [])
  // Empty api_key means "keep the stored secret unchanged".
  const [api_key, setApiKey] = useState("")
  const [is_fallback, setIsFallback] = useState(!!provider?.is_fallback)

  const handleSubmit = () => {
    const body: ProviderUpdate = {
      label,
      base_url,
      chat_models,
      is_fallback,
      ...(api_key.trim() ? { api_key } : {}),
    }
    onSubmit(body)
  }

  return (
    <Dialog open={provider !== null} onOpenChange={(open) => { if (!open) onClose() }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            Edit provider
            {provider && (
              <Badge variant="outline" className="ml-2 font-mono">
                {provider.name}
              </Badge>
            )}
          </DialogTitle>
        </DialogHeader>
        {provider && (
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="e-label">Label</Label>
              <Input
                id="e-label"
                placeholder="Personal"
                value={label}
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="e-base">Base URL</Label>
              <Input
                id="e-base"
                placeholder="https://api.openai.com/v1"
                value={base_url}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
            </div>
            <ChatModelsPicker
              id="e-chat-models"
              mode="saved"
              providerId={provider.id}
              value={chat_models}
              onChange={setChatModels}
            />
            <div className="space-y-1.5">
              <Label htmlFor="e-key">
                API key{" "}
                <span className="text-xs font-normal text-muted-foreground">
                  (leave blank to keep current: {provider.api_key_hint || "none"})
                </span>
              </Label>
              <Textarea
                id="e-key"
                placeholder="sk-…"
                value={api_key}
                onChange={(e) => setApiKey(e.target.value)}
                className="font-mono text-xs"
                rows={2}
              />
            </div>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={is_fallback}
                onChange={(e) => setIsFallback(e.target.checked)}
                className="h-4 w-4 rounded border-input"
              />
              <span>
                Use as <strong>fallback</strong> provider
              </span>
            </label>
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={handleSubmit} disabled={pending}>
            {pending && <Loader2 className="h-4 w-4 animate-spin" />}
            Save changes
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// --- Global agent configuration ---

/**
 * Global agent defaults: tool-permission matrix, capability policy, and
 * breakpoints. Persisted in localStorage and applied to every newly created
 * conversation (see Sidebar). Per-conversation overrides set from the chat
 * composer's mode picker still take precedence.
 */
function AgentConfigSection() {
  const [perms, setPerms] = useState<ToolPermissions>(
    () => loadAgentDefaults().permissions
  )
  const [capPolicy, setCapPolicy] = useState<CapabilityPolicy>(
    () => loadAgentDefaults().capabilityPolicy
  )
  const [bpList, setBpList] = useState<BreakpointConfig[]>(
    () => loadAgentDefaults().breakpoints
  )
  const [toolQuery, setToolQuery] = useState("")
  const [changedOnly, setChangedOnly] = useState(false)
  const filteredTools = useMemo(() => {
    const query = toolQuery.trim().toLocaleLowerCase()
    return TOOL_NAMES.filter(
      (tool) =>
        (!changedOnly || perms[tool] !== undefined) &&
        (!query || tool.toLocaleLowerCase().includes(query))
    )
  }, [changedOnly, perms, toolQuery])
  const advancedChangeCount =
    Object.keys(perms).length + Object.keys(capPolicy).length + bpList.length

  const cycle = (tool: string) => {
    setPerms((cur) => {
      const current = (cur[tool] ?? "ask") as ToolPermission
      const nextIdx = (PERMISSIONS.indexOf(current) + 1) % PERMISSIONS.length
      return { ...cur, [tool]: PERMISSIONS[nextIdx] }
    })
  }

  const cycleCap = (cap: string) => {
    setCapPolicy((cur) => {
      const current = (cur[cap] ?? "allow") as ToolPermission
      const nextIdx = (PERMISSIONS.indexOf(current) + 1) % PERMISSIONS.length
      return { ...cur, [cap]: PERMISSIONS[nextIdx] }
    })
  }

  const toggleBreakpoint = (type: BreakpointType) => {
    setBpList((cur) => {
      const exists = cur.some((bp) => bp.type === type)
      if (exists) return cur.filter((bp) => bp.type !== type)
      return [...cur, { type, fallback: "deny" as const }]
    })
  }

  const handleSave = () => {
    saveAgentDefaults({ permissions: perms, capabilityPolicy: capPolicy, breakpoints: bpList })
    toast.success("Agent defaults saved", {
      description: "Applied to newly created conversations.",
    })
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <ShieldCheck className="h-4 w-4" />
          </div>
          <div>
            <CardTitle className="text-lg">Agent</CardTitle>
            <CardDescription>
              Default tool permissions, capability gates, and breakpoints for
              new conversations.
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-5">
        <div className="space-y-2">
          <Label>Safety preset</Label>
          <p className="text-xs text-muted-foreground">
            Choose the default posture for new conversations. You can still override individual
            tools below.
          </p>
          <div className="grid gap-2 sm:grid-cols-3">
            {MODE_LABELS.map(({ mode, label, hint }) => {
              const active =
                JSON.stringify(perms) === JSON.stringify(MODE_PRESETS[mode])
              return (
                <button
                  key={mode}
                  type="button"
                  onClick={() => setPerms({ ...MODE_PRESETS[mode] })}
                  className={cn(
                    "min-h-16 rounded-lg border px-3 py-2 text-left transition-colors",
                    active
                      ? "border-primary bg-primary/5"
                      : "hover:bg-muted"
                  )}
                >
                  <span className="block text-sm font-medium">{label}</span>
                  <span className="mt-1 block text-xs leading-5 text-muted-foreground">{hint}</span>
                </button>
              )
            })}
          </div>
        </div>

        <details className="group rounded-lg border" open={advancedChangeCount > 1}>
          <summary className="flex min-h-11 cursor-pointer list-none items-center gap-2 px-3 text-sm font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
            Advanced overrides
            <span className="ml-auto text-xs font-normal text-muted-foreground">
              {advancedChangeCount} configured
            </span>
            <ChevronRight className="h-4 w-4 transition-transform group-open:rotate-90" />
          </summary>
          <div className="space-y-5 border-t p-3 sm:p-4">
        {/* Tool permissions */}
        <div className="space-y-2">
          <Label>Tool permissions</Label>
          <p className="text-xs text-muted-foreground">
            Click a cell to cycle: allow → ask → deny. The “*” row is the
            default for any tool not listed.
          </p>
          <div className="flex flex-col gap-2 sm:flex-row">
            <div className="relative min-w-0 flex-1">
              <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={toolQuery}
                onChange={(event) => setToolQuery(event.target.value)}
                placeholder="Search tools"
                aria-label="Search tool permissions"
                className="pl-9 text-base sm:text-sm"
              />
            </div>
            <label className="flex min-h-10 items-center gap-2 rounded-md border px-3 text-sm">
              <input
                type="checkbox"
                checked={changedOnly}
                onChange={(event) => setChangedOnly(event.target.checked)}
              />
              Changed only
            </label>
          </div>
          <div className="rounded-md border">
            {filteredTools.map((tool, i) => {
              const value = (perms[tool] ?? (tool === "*" ? "ask" : "inherit")) as
                | ToolPermission
                | "inherit"
              return (
                <div
                  key={tool}
                  className={cn(
                    "flex items-center justify-between px-3 py-2",
                    i > 0 && "border-t"
                  )}
                >
                  <span className="font-mono text-xs">{tool}</span>
                  <button
                    type="button"
                    onClick={() => cycle(tool)}
                    className={cn(
                      "min-h-8 rounded px-3 py-1 text-xs font-medium capitalize transition-colors",
                      value === "inherit"
                        ? "bg-muted text-muted-foreground"
                        : PERM_STYLES[value]
                    )}
                  >
                    {value}
                  </button>
                </div>
              )
            })}
            {filteredTools.length === 0 && (
              <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                No tool permissions match this filter.
              </p>
            )}
          </div>
        </div>

        {/* Capability policy */}
        <div className="space-y-2">
          <Label>Capability policy</Label>
          <p className="text-xs text-muted-foreground">
            Coarse-grained gates applied before per-tool permissions. The more
            restrictive of the two layers wins. Click to cycle.
          </p>
          <div className="rounded-md border">
            {CAPABILITY_NAMES.map((cap, i) => {
              const value = (capPolicy[cap] ?? (cap === "*" ? "allow" : "inherit")) as
                | ToolPermission
                | "inherit"
              return (
                <div
                  key={cap}
                  className={cn(
                    "flex items-center justify-between px-3 py-2",
                    i > 0 && "border-t"
                  )}
                >
                  <span className="font-mono text-xs">{cap}</span>
                  <button
                    type="button"
                    onClick={() => cycleCap(cap)}
                    className={cn(
                      "rounded px-2 py-0.5 text-xs font-medium capitalize transition-colors",
                      value === "inherit"
                        ? "bg-muted text-muted-foreground"
                        : PERM_STYLES[value]
                    )}
                  >
                    {value}
                  </button>
                </div>
              )
            })}
          </div>
        </div>

        {/* Breakpoints */}
        <div className="space-y-2">
          <Label>Breakpoints</Label>
          <p className="text-xs text-muted-foreground">
            Pause the agent at key points for human review. The agent blocks
            until you approve or the timeout fires.
          </p>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {BREAKPOINT_TYPES.map(({ type, label, hint }) => {
              const active = bpList.some((bp) => bp.type === type)
              return (
                <button
                  key={type}
                  type="button"
                  onClick={() => toggleBreakpoint(type)}
                  className={cn(
                    "rounded-md border px-2 py-1.5 text-left transition-colors",
                    active
                      ? "border-primary bg-primary/10"
                      : "hover:bg-muted"
                  )}
                >
                  <div className="text-xs font-medium">{label}</div>
                  <div className="text-[10px] text-muted-foreground">{hint}</div>
                </button>
              )
            })}
          </div>
        </div>
          </div>
        </details>

        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-between">
          <Button
            variant="ghost"
            onClick={() => {
              setPerms({ ...MODE_PRESETS.ask })
              setCapPolicy({})
              setBpList([])
            }}
          >
            <RotateCcw className="h-4 w-4" /> Reset to safe defaults
          </Button>
          <Button onClick={handleSave}>Save agent defaults</Button>
        </div>
      </CardContent>
    </Card>
  )
}

// --- Skills management ---

/**
 * Global skills section: displays all available skills (builtin + user) and
 * allows deleting user-created skills. These skills are enabled for all
 * projects by default; per-project disabling is done in ProjectSettingsDialog.
 */
function SkillsSection() {
  const queryClient = useQueryClient()

  const { data, isLoading } = useQuery({
    queryKey: ["skills"],
    queryFn: () => skillsApi.list(),
  })

  const deleteMutation = useMutation({
    mutationFn: skillsApi.delete,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["skills"] })
      toast.success("Skill deleted")
    },
    onError: () => toast.error("Skill was not deleted", {
      description: "Built-in skills cannot be removed. Refresh the list and try again.",
    }),
  })

  const skills: SkillInfo[] = data?.skills ?? []

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <Sparkles className="h-4 w-4" />
          </div>
          <div>
            <CardTitle className="text-lg">Skills</CardTitle>
            <CardDescription>
              Reusable AI capability modules. Global skills are enabled for all
              projects by default.
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {isLoading ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : skills.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">
            No skills available.
          </p>
        ) : (
          <div className="rounded-md border">
            {skills.map((skill, i) => (
              <div
                key={skill.name}
                className={cn(
                  "flex items-center justify-between px-3 py-2.5",
                  i > 0 && "border-t"
                )}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-xs font-medium">{skill.name}</span>
                    <Badge variant="outline" className="text-[10px]">
                      {skill.source}
                    </Badge>
                  </div>
                  <p className="mt-0.5 truncate text-xs text-muted-foreground">
                    {skill.description || "No description"}
                  </p>
                  {skill.tags.length > 0 && (
                    <div className="mt-1 flex flex-wrap gap-1">
                      {skill.tags.map((tag) => (
                        <span
                          key={tag}
                          className="rounded bg-muted px-1 py-0.5 text-[10px] text-muted-foreground"
                        >
                          {tag}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
                {skill.source !== "builtin" && (
                  <Button
                    variant="ghost"
                    size="icon"
                    className="ml-2 shrink-0 text-muted-foreground hover:text-destructive"
                    onClick={() => deleteMutation.mutate(skill.name)}
                    disabled={deleteMutation.isPending}
                    title="Delete skill"
                  >
                    {deleteMutation.isPending ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Trash2 className="h-3.5 w-3.5" />
                    )}
                  </Button>
                )}
              </div>
            ))}
          </div>
        )}
        <p className="text-xs text-muted-foreground">
          Builtin skills cannot be deleted. Create new skills via the agent
          using the <code className="rounded bg-muted px-1">create_skill</code> tool
          or the <code className="rounded bg-muted px-1">skill-creation</code> skill.
        </p>
      </CardContent>
    </Card>
  )
}

// --- MCP servers management ---

function explainMcpError(error: string) {
  if (error.includes("WinError 2") || error.includes("Failed to spawn")) {
    return {
      summary: "The server command could not be found.",
      recovery: "Check the command and arguments, then reconnect the server.",
    }
  }
  if (error.toLocaleLowerCase().includes("connection")) {
    return {
      summary: "The server did not accept the connection.",
      recovery: "Confirm that it is running and that its URL or transport is correct.",
    }
  }
  return {
    summary: "The server could not be connected.",
    recovery: "Review its configuration, then try reconnecting.",
  }
}

const EMPTY_MCP_FORM: MCPServerCreate = {
  name: "",
  transport: "stdio",
  command: "",
  args: [],
  url: "",
  description: "",
  enabled: true,
}

/**
 * MCP servers section: displays configured MCP servers, their connection
 * status, discovered tools, and allows adding/removing/connecting servers.
 */
function MCPServersSection() {
  const queryClient = useQueryClient()
  const [addOpen, setAddOpen] = useState(false)
  const [form, setForm] = useState<MCPServerCreate>(EMPTY_MCP_FORM)
  const [argsText, setArgsText] = useState("")

  const { data, isLoading } = useQuery({
    queryKey: ["mcp-servers"],
    queryFn: () => mcpApi.listServers(),
  })

  const addMutation = useMutation({
    mutationFn: mcpApi.addServer,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("MCP server added")
      setAddOpen(false)
      setForm(EMPTY_MCP_FORM)
      setArgsText("")
    },
    onError: () => toast.error("MCP server could not be added", {
      description: "Check the transport, command or URL, and required fields.",
    }),
  })

  const removeMutation = useMutation({
    mutationFn: mcpApi.removeServer,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("MCP server removed")
    },
    onError: () => toast.error("MCP server was not removed", {
      description: "Disconnect it first, then try again.",
    }),
  })

  const connectMutation = useMutation({
    mutationFn: mcpApi.connect,
    onSuccess: (res) => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      if (res.status === "connected") {
        toast.success(`Connected to ${res.name}`, { description: `${res.tools_count} tools discovered` })
      } else {
        const explanation = explainMcpError(res.error ?? "")
        toast.error(`${res.name} could not connect`, { description: explanation.recovery })
      }
    },
    onError: () => toast.error("MCP server could not connect", {
      description: "Check that the server is running and its configuration is correct.",
    }),
  })

  const disconnectMutation = useMutation({
    mutationFn: mcpApi.disconnect,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("Server disconnected")
    },
    onError: () => toast.error("MCP server did not disconnect", {
      description: "Wait a moment, then try again.",
    }),
  })

  const reconnectAllMutation = useMutation({
    mutationFn: mcpApi.reconnectAll,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("All servers reconnected")
    },
    onError: () => toast.error("Servers could not be reconnected", {
      description: "Review the servers marked with an error and reconnect them individually.",
    }),
  })

  const handleAdd = () => {
    if (!form.name.trim()) {
      toast.error("Server name is required")
      return
    }
    const body = {
      ...form,
      args: argsText.split(/\s+/).filter(Boolean),
    }
    addMutation.mutate(body)
  }

  const servers: MCPServer[] = data?.servers ?? []

  return (
    <Card>
      <CardHeader>
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <Server className="h-4 w-4" />
            </div>
            <div>
              <CardTitle className="text-lg">MCP Servers</CardTitle>
              <CardDescription>
                Model Context Protocol servers extend the agent with external
                tools. Configure stdio or HTTP connections.
              </CardDescription>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              className="gap-1.5"
              onClick={() => reconnectAllMutation.mutate()}
              disabled={reconnectAllMutation.isPending || servers.length === 0}
            >
              {reconnectAllMutation.isPending ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              Reconnect all
            </Button>
            <Dialog open={addOpen} onOpenChange={setAddOpen}>
              <DialogTrigger asChild>
                <Button size="sm" className="gap-1.5">
                  <Plus className="h-3.5 w-3.5" /> Add server
                </Button>
              </DialogTrigger>
              <DialogContent>
                <DialogHeader>
                  <DialogTitle>Add MCP server</DialogTitle>
                </DialogHeader>
                <div className="space-y-3">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                    <div className="space-y-1.5">
                      <Label htmlFor="mcp-name">Name</Label>
                      <Input
                        id="mcp-name"
                        placeholder="my-server"
                        value={form.name}
                        onChange={(e) => setForm({ ...form, name: e.target.value })}
                      />
                    </div>
                    <div className="space-y-1.5">
                      <Label htmlFor="mcp-transport">Transport</Label>
                      <select
                        id="mcp-transport"
                        className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                        value={form.transport}
                        onChange={(e) => setForm({ ...form, transport: e.target.value as "stdio" | "http" })}
                      >
                        <option value="stdio">stdio (subprocess)</option>
                        <option value="http">HTTP</option>
                      </select>
                    </div>
                  </div>
                  {form.transport === "stdio" ? (
                    <>
                      <div className="space-y-1.5">
                        <Label htmlFor="mcp-cmd">Command</Label>
                        <Input
                          id="mcp-cmd"
                          placeholder="npx"
                          value={form.command}
                          onChange={(e) => setForm({ ...form, command: e.target.value })}
                        />
                      </div>
                      <div className="space-y-1.5">
                        <Label htmlFor="mcp-args">Arguments (space-separated)</Label>
                        <Input
                          id="mcp-args"
                          placeholder="-y @modelcontextprotocol/server-filesystem /tmp"
                          value={argsText}
                          onChange={(e) => setArgsText(e.target.value)}
                        />
                      </div>
                    </>
                  ) : (
                    <div className="space-y-1.5">
                      <Label htmlFor="mcp-url">URL</Label>
                      <Input
                        id="mcp-url"
                        placeholder="http://localhost:8080/mcp"
                        value={form.url}
                        onChange={(e) => setForm({ ...form, url: e.target.value })}
                      />
                    </div>
                  )}
                  <div className="space-y-1.5">
                    <Label htmlFor="mcp-desc">Description</Label>
                    <Input
                      id="mcp-desc"
                      placeholder="Filesystem access tools"
                      value={form.description}
                      onChange={(e) => setForm({ ...form, description: e.target.value })}
                    />
                  </div>
                </div>
                <DialogFooter>
                  <Button variant="outline" onClick={() => setAddOpen(false)}>
                    Cancel
                  </Button>
                  <Button onClick={handleAdd} disabled={addMutation.isPending}>
                    {addMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                    Add server
                  </Button>
                </DialogFooter>
              </DialogContent>
            </Dialog>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {isLoading ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : servers.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">
            No MCP servers configured. Add one to extend the agent with external tools.
          </p>
        ) : (
          <div className="rounded-md border">
            {servers.map((server, i) => {
              const friendlyError = server.error ? explainMcpError(server.error) : null
              return (
              <div
                key={server.name}
                className={cn(
                  "px-3 py-2.5",
                  i > 0 && "border-t"
                )}
              >
                <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <span className="font-mono text-xs font-medium">{server.name}</span>
                    <Badge variant="outline" className="text-[10px]">
                      {server.transport}
                    </Badge>
                    {server.status === "connected" ? (
                      <Badge variant="success" className="gap-1">
                        <CheckCircle2 className="h-3 w-3" /> connected
                      </Badge>
                    ) : server.status === "error" ? (
                      <Badge variant="destructive">error</Badge>
                    ) : server.status === "connecting" ? (
                      <Badge variant="secondary" className="gap-1">
                        <Loader2 className="h-3 w-3 animate-spin" /> connecting
                      </Badge>
                    ) : (
                      <Badge variant="outline">disconnected</Badge>
                    )}
                    {!server.enabled && <Badge variant="warning">disabled</Badge>}
                  </div>
                  <div className="flex items-center gap-1 self-end sm:self-auto">
                    {server.status === "connected" ? (
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-9 w-9 text-muted-foreground hover:text-foreground"
                        onClick={() => disconnectMutation.mutate(server.name)}
                        disabled={disconnectMutation.isPending}
                        title={`Disconnect ${server.name}`}
                        aria-label={`Disconnect ${server.name}`}
                      >
                        <Unplug className="h-3.5 w-3.5" />
                      </Button>
                    ) : (
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-9 w-9 text-muted-foreground hover:text-foreground"
                        onClick={() => connectMutation.mutate(server.name)}
                        disabled={connectMutation.isPending}
                        title={`Connect ${server.name}`}
                        aria-label={`Connect ${server.name}`}
                      >
                        <Plug className="h-3.5 w-3.5" />
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-9 w-9 text-muted-foreground hover:text-destructive"
                      onClick={() => removeMutation.mutate(server.name)}
                      disabled={removeMutation.isPending}
                      title={`Remove ${server.name}`}
                      aria-label={`Remove ${server.name}`}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
                {server.description && (
                  <p className="mt-0.5 text-xs text-muted-foreground">{server.description}</p>
                )}
                {friendlyError && server.error && (
                  <div className="mt-2 rounded-md bg-destructive/10 px-3 py-2 text-sm text-destructive">
                    <p className="font-medium">{friendlyError.summary}</p>
                    <p className="mt-0.5 text-xs leading-5">{friendlyError.recovery}</p>
                    <details className="mt-1 text-xs">
                      <summary className="cursor-pointer font-medium">Technical details</summary>
                      <code className="mt-1 block break-all text-[11px] leading-4">{server.error}</code>
                    </details>
                  </div>
                )}
                {server.tools.length > 0 && (
                  <div className="mt-1.5 flex flex-wrap gap-1">
                    {server.tools.map((tool) => (
                      <span
                        key={tool.qualified_name}
                        className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-mono text-muted-foreground"
                        title={tool.description}
                      >
                        {tool.name}
                      </span>
                    ))}
                  </div>
                )}
              </div>
              )
            })}
          </div>
        )}
        <p className="text-xs text-muted-foreground">
          Configure servers via the UI or <code className="rounded bg-muted px-1">config.yaml</code> at
          the project root. Tools from connected servers are automatically available to the agent.
        </p>
      </CardContent>
    </Card>
  )
}

// --- MCP Store ---

/**
 * MCP Store section: browse and install MCP servers from the official
 * MCP Registry (registry.modelcontextprotocol.io).
 */
function MCPStoreSection() {
  const queryClient = useQueryClient()
  const [searchQuery, setSearchQuery] = useState("")
  const [submittedQuery, setSubmittedQuery] = useState("")

  const { data, isLoading, isFetching } = useQuery({
    queryKey: ["mcp-store", submittedQuery],
    queryFn: () =>
      submittedQuery
        ? mcpApi.storeSearch(submittedQuery, 10)
        : mcpApi.storePopular(10),
  })

  const installMutation = useMutation({
    mutationFn: mcpApi.storeInstall,
    onSuccess: (res) => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      if (res.status === "connected") {
        toast.success(`Installed ${res.name}`, {
          description: `${res.tools_count} tools available`,
        })
      } else {
        const explanation = res.error ? explainMcpError(res.error) : null
        toast.success(`Installed ${res.name}`, {
          description: explanation?.summary ?? "Server configured but not connected.",
        })
      }
    },
    onError: () => toast.error("MCP server could not be installed", {
      description: "Check the registry connection and try again.",
    }),
  })

  const results: MCPStoreItem[] = data?.results ?? []

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <Store className="h-4 w-4" />
          </div>
          <div>
            <CardTitle className="text-lg">MCP Store</CardTitle>
            <CardDescription>
              Browse and install MCP servers from the official{" "}
              <a
                href="https://registry.modelcontextprotocol.io"
                target="_blank"
                rel="noreferrer"
                className="underline underline-offset-2"
              >
                MCP Registry
              </a>
              .
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {/* Search bar */}
        <form
          className="flex gap-2"
          onSubmit={(e) => {
            e.preventDefault()
            setSubmittedQuery(searchQuery)
          }}
        >
          <div className="relative flex-1">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input
              placeholder="Search MCP servers (e.g. filesystem, github, sql…)"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="pl-8"
            />
          </div>
          <Button type="submit" variant="outline" size="sm" className="shrink-0">
            Search
          </Button>
        </form>

        {/* Results */}
        {isLoading || isFetching ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : results.length === 0 ? (
          <p className="py-4 text-center text-sm text-muted-foreground">
            {submittedQuery
              ? `No servers found for “${submittedQuery}”.`
              : "No servers available from the registry."}
          </p>
        ) : (
          <div className="rounded-md border">
            {results.map((item, i) => (
              <div
                key={item.name}
                className={cn(
                  "flex items-center justify-between px-3 py-2.5",
                  i > 0 && "border-t"
                )}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-mono text-xs font-medium">
                      {item.name.split("/").pop() ?? item.name}
                    </span>
                    {item.version && (
                      <Badge variant="outline" className="text-[10px]">
                        v{item.version}
                      </Badge>
                    )}
                    {item.transport && (
                      <Badge variant="secondary" className="text-[10px]">
                        {item.transport}
                      </Badge>
                    )}
                  </div>
                  <p className="mt-0.5 truncate text-xs text-muted-foreground">
                    {item.description || "No description"}
                  </p>
                  {item.install_command && (
                    <p className="mt-0.5 truncate font-mono text-[10px] text-muted-foreground">
                      {item.install_command}
                    </p>
                  )}
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  className="ml-2 shrink-0 gap-1.5"
                  onClick={() =>
                    installMutation.mutate({ registry_name: item.name })
                  }
                  disabled={installMutation.isPending}
                >
                  {installMutation.isPending &&
                  installMutation.variables?.registry_name === item.name ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Download className="h-3.5 w-3.5" />
                  )}
                  Install
                </Button>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- System prompt editor ---

/**
 * System prompt editor: view and customize the default system prompt sent
 * to the LLM on every agent run. Supports resetting to the built-in default.
 */
function SystemPromptSection() {
  const { data, isLoading } = useQuery({
    queryKey: ["system-prompt"],
    queryFn: settingsApi.getSystemPrompt,
  })

  const [prompt, setPrompt] = useState<string | null>(null)
  const [dirty, setDirty] = useState(false)

  // Seed the textarea once data arrives.
  const effectiveValue = prompt ?? data?.prompt ?? ""

  const saveMutation = useMutation({
    mutationFn: (value: string) => settingsApi.updateSystemPrompt({ prompt: value }),
    onSuccess: (res) => {
      setDirty(false)
      toast.success("System prompt updated", {
        description: res.is_custom ? "Custom prompt active." : "Reset to built-in default.",
      })
    },
    onError: () => toast.error("System prompt was not saved", {
      description: "Your text is still here. Check the connection and try again.",
    }),
  })

  const handleSave = () => {
    saveMutation.mutate(effectiveValue)
  }

  const handleReset = () => {
    setPrompt("")
    setDirty(true)
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <FileText className="h-4 w-4" />
          </div>
          <div>
            <CardTitle className="text-lg">System Prompt</CardTitle>
            <CardDescription>
              The default instruction set sent to the LLM on every agent run.
              {data && (
                <span className="ml-1">
                  Source: <Badge variant="outline" className="font-mono text-[10px]">{data.source}</Badge>
                </span>
              )}
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-3">
        {isLoading ? (
          <div className="flex justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : (
          <>
            <Textarea
              value={effectiveValue}
              onChange={(e) => {
                setPrompt(e.target.value)
                setDirty(true)
              }}
              rows={16}
              className="font-mono text-xs leading-relaxed"
              placeholder="Enter your custom system prompt…"
            />
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleReset}
                  className="gap-1.5"
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                  Reset to default
                </Button>
                {data?.is_custom && (
                  <Badge variant="secondary">customized</Badge>
                )}
              </div>
              <Button
                onClick={handleSave}
                disabled={!dirty || saveMutation.isPending}
                className="gap-1.5"
              >
                {saveMutation.isPending && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                Save prompt
              </Button>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}
