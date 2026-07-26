import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { KeyRound, Plus, Trash2, Loader2, CheckCircle2, Pencil, ShieldCheck, FileText, RotateCcw, Star, Sparkles, Plug, Unplug, RefreshCw, Server, Search, Download, Store } from "lucide-react"
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
  const [createOpen, setCreateOpen] = useState(false)
  const [createForm, setCreateForm] = useState<ProviderCreate>(EMPTY_FORM)
  const [editing, setEditing] = useState<Provider | null>(null)

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
    onError: (e) => toast.error("Failed to add provider", { description: String(e) }),
  })

  const updateMutation = useMutation({
    mutationFn: (vars: { id: number; body: ProviderUpdate }) =>
      providersApi.update(vars.id, vars.body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider updated")
      setEditing(null)
    },
    onError: (e) => toast.error("Failed to update provider", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: providersApi.delete,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider deleted")
    },
    onError: (e) => toast.error("Failed to delete", { description: String(e) }),
  })

  const handleCreate = () => {
    if (!createForm.api_key.trim()) {
      toast.error("API key is required")
      return
    }
    createMutation.mutate(createForm)
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl space-y-6 p-6">
        <header className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <KeyRound className="h-4 w-4" />
            </div>
            <div>
              <h1 className="text-lg font-semibold">Providers</h1>
              <p className="text-sm text-muted-foreground">
                Manage API keys for LLM providers. Keys are encrypted at rest.
              </p>
            </div>
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
                  Save
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </header>

        {isLoading ? (
          <div className="flex justify-center py-12">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : providers.length === 0 ? (
          <Card>
            <CardContent className="py-12 text-center text-sm text-muted-foreground">
              No providers yet. Click <strong>Add provider</strong> to configure one.
            </CardContent>
          </Card>
        ) : (
          <div className="space-y-3">
            {providers.map((p) => (
              <ProviderRow
                key={p.id}
                provider={p}
                onEdit={() => setEditing(p)}
                onDelete={() => deleteMutation.mutate(p.id)}
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

        {/* Global agent configuration (defaults for new conversations). */}
        <AgentConfigSection />

        {/* Skills management */}
        <SkillsSection />

        {/* MCP servers management */}
        <MCPServersSection />

        {/* MCP Store */}
        <MCPStoreSection />

        {/* System prompt editor */}
        <SystemPromptSection />
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
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
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
          <div className="flex items-center gap-2">
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
              title="Edit"
            >
              <Pencil className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="text-muted-foreground hover:text-destructive"
              onClick={onDelete}
              disabled={deleting}
              title="Delete"
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
        {/* Tool permissions */}
        <div className="space-y-2">
          <Label>Tool permissions</Label>
          <p className="text-xs text-muted-foreground">
            Click a cell to cycle: allow → ask → deny. The “*” row is the
            default for any tool not listed.
          </p>
          <div className="rounded-md border">
            {TOOL_NAMES.map((tool, i) => {
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
          <div className="grid grid-cols-2 gap-2">
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

        <div className="flex justify-end">
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
    onError: (e) => toast.error("Failed to delete skill", { description: String(e) }),
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
    onError: (e) => toast.error("Failed to add MCP server", { description: String(e) }),
  })

  const removeMutation = useMutation({
    mutationFn: mcpApi.removeServer,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("MCP server removed")
    },
    onError: (e) => toast.error("Failed to remove server", { description: String(e) }),
  })

  const connectMutation = useMutation({
    mutationFn: mcpApi.connect,
    onSuccess: (res) => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      if (res.status === "connected") {
        toast.success(`Connected to ${res.name}`, { description: `${res.tools_count} tools discovered` })
      } else {
        toast.error(`Failed to connect ${res.name}`, { description: res.error ?? undefined })
      }
    },
    onError: (e) => toast.error("Connection failed", { description: String(e) }),
  })

  const disconnectMutation = useMutation({
    mutationFn: mcpApi.disconnect,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("Server disconnected")
    },
    onError: (e) => toast.error("Disconnect failed", { description: String(e) }),
  })

  const reconnectAllMutation = useMutation({
    mutationFn: mcpApi.reconnectAll,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp-servers"] })
      toast.success("All servers reconnected")
    },
    onError: (e) => toast.error("Reconnect failed", { description: String(e) }),
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
        <div className="flex items-center justify-between">
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
          <div className="flex items-center gap-2">
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
                  <div className="grid grid-cols-2 gap-3">
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
            {servers.map((server, i) => (
              <div
                key={server.name}
                className={cn(
                  "px-3 py-2.5",
                  i > 0 && "border-t"
                )}
              >
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
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
                  <div className="flex items-center gap-1">
                    {server.status === "connected" ? (
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7 text-muted-foreground hover:text-foreground"
                        onClick={() => disconnectMutation.mutate(server.name)}
                        disabled={disconnectMutation.isPending}
                        title="Disconnect"
                      >
                        <Unplug className="h-3.5 w-3.5" />
                      </Button>
                    ) : (
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7 text-muted-foreground hover:text-foreground"
                        onClick={() => connectMutation.mutate(server.name)}
                        disabled={connectMutation.isPending}
                        title="Connect"
                      >
                        <Plug className="h-3.5 w-3.5" />
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7 text-muted-foreground hover:text-destructive"
                      onClick={() => removeMutation.mutate(server.name)}
                      disabled={removeMutation.isPending}
                      title="Remove"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
                {server.description && (
                  <p className="mt-0.5 text-xs text-muted-foreground">{server.description}</p>
                )}
                {server.error && (
                  <p className="mt-0.5 text-xs text-destructive">{server.error}</p>
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
            ))}
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
        toast.success(`Installed ${res.name}`, {
          description: res.error ?? "Server configured (not connected)",
        })
      }
    },
    onError: (e) => toast.error("Install failed", { description: String(e) }),
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
    onError: (e) => toast.error("Failed to save system prompt", { description: String(e) }),
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
