import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Cpu, Loader2, Plug, Server, ShieldAlert, ShieldCheck, Wrench } from "lucide-react"
import { toast } from "sonner"
import {
  extensionsApi,
  type HookRecord,
  type McpServerRecord,
  type McpToolPolicyRecord,
  type PluginRecord,
  type SkillRecord,
  type WorkerRecord,
} from "@/api/extensions"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { QueryErrorState, QueryLoadingState } from "@/components/ui/query-state"

export function ExtensionsPage() {
  const queryClient = useQueryClient()
  const { data, isLoading, isError, refetch } = useQuery({
    queryKey: ["extensions"],
    queryFn: extensionsApi.status,
  })

  const enableMutation = useMutation({
    mutationFn: ({ plugin, enabled }: { plugin: string; enabled: boolean }) =>
      extensionsApi.setPluginEnabled(plugin, enabled),
    onSuccess: (_record, variables) => {
      queryClient.invalidateQueries({ queryKey: ["extensions"] })
      toast.success(variables.enabled ? "Plugin enabled" : "Plugin disabled", {
        description: "The change applies when the extension host next starts.",
      })
    },
    onError: (error) =>
      toast.error("Plugin state was not changed", {
        description: extensionErrorMessage(error, "Refresh and try again."),
      }),
  })

  const reviewMutation = useMutation({
    mutationFn: ({
      plugin,
      hook,
      trustHash,
      approved,
    }: {
      plugin: string
      hook: string
      trustHash: string
      approved: boolean
    }) => extensionsApi.setHookReview(plugin, hook, trustHash, approved),
    onSuccess: (_record, variables) => {
      queryClient.invalidateQueries({ queryKey: ["extensions"] })
      toast.success(variables.approved ? "Hook approved" : "Hook approval revoked", {
        description: "The change applies when the extension host next starts.",
      })
    },
    onError: (error) =>
      toast.error("Hook review was not saved", {
        description: extensionErrorMessage(error, "Refresh and try again."),
      }),
  })

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl space-y-6 p-6">
        <header className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
            <Plug className="h-4 w-4" />
          </div>
          <div>
            <h1 className="text-lg font-semibold">Extensions &amp; Workers</h1>
            <p className="text-sm text-muted-foreground">
              Installed plugins, supervised compatibility workers and the hook
              permission review queue. Extensions are untrusted: they can only
              narrow the core policy, never widen it.
            </p>
          </div>
        </header>

        {isLoading ? (
          <QueryLoadingState label="Loading extension status…" />
        ) : isError || !data ? (
          <QueryErrorState
            title="Extension status could not be loaded"
            description="The Rust extension host may not be configured. Check that Cool is running, then try again."
            onRetry={() => void refetch()}
          />
        ) : (
          <>
            <PluginsCard
              plugins={data.plugins}
              pendingPlugin={
                enableMutation.isPending
                  ? (enableMutation.variables?.plugin ?? null)
                  : null
              }
              onToggle={(plugin, enabled) => enableMutation.mutate({ plugin, enabled })}
            />
            <WorkersCard workers={data.workers} />
            <HookReviewCard
              hooks={data.hooks}
              pendingKey={
                reviewMutation.isPending && reviewMutation.variables
                  ? `${reviewMutation.variables.plugin}/${reviewMutation.variables.hook}`
                  : null
              }
              onReview={(hook, approved) =>
                reviewMutation.mutate({
                  plugin: hook.plugin,
                  hook: hook.id,
                  trustHash: hook.trustHash,
                  approved,
                })
              }
            />
            <SkillsCard skills={data.skills} />
            <McpCard servers={data.mcpServers} policy={data.mcpToolPolicy} />
          </>
        )}
      </div>
    </div>
  )
}

function EmptyNote({ children }: { children: string }) {
  return <p className="py-4 text-center text-sm text-muted-foreground">{children}</p>
}

function PluginsCard({
  plugins,
  pendingPlugin,
  onToggle,
}: {
  plugins: PluginRecord[]
  /** Plugin name whose enable/disable mutation is in flight, if any. */
  pendingPlugin: string | null
  onToggle: (plugin: string, enabled: boolean) => void
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Wrench className="h-4 w-4" /> Plugins
        </CardTitle>
        <CardDescription>
          Declarative skill/MCP/hook bundles. Enabling re-verifies the content
          hash; a tampered tree fails closed.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {plugins.length === 0 ? (
          <EmptyNote>No plugins are installed for this host.</EmptyNote>
        ) : (
          plugins.map((plugin) => {
            const pending = pendingPlugin === plugin.name
            return (
            <div key={plugin.name} className="rounded-md border p-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{plugin.name}</span>
                <span className="text-xs text-muted-foreground">{plugin.version || "—"}</span>
                <Badge variant={plugin.enabled ? "success" : "secondary"}>
                  {plugin.enabled ? "enabled" : "disabled"}
                </Badge>
                <Badge variant={plugin.contentVerified ? "outline" : "destructive"}>
                  {plugin.contentVerified ? "verified" : "tampered"}
                </Badge>
                <Button
                  size="sm"
                  variant="outline"
                  className="ml-auto gap-1.5"
                  disabled={pending || (!plugin.enabled && !plugin.contentVerified)}
                  aria-busy={pending}
                  title={
                    !plugin.enabled && !plugin.contentVerified
                      ? "Reinstall the plugin before enabling it"
                      : undefined
                  }
                  onClick={() => onToggle(plugin.name, !plugin.enabled)}
                >
                  {pending && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                  {plugin.enabled ? "Disable" : "Enable"}
                </Button>
              </div>
              <dl className="mt-2 grid gap-1 text-xs text-muted-foreground sm:grid-cols-2">
                <div>
                  <span className="font-medium text-foreground">Source:</span>{" "}
                  {plugin.sourceType} · {plugin.source || "—"}
                </div>
                <div>
                  <span className="font-medium text-foreground">Content hash:</span>{" "}
                  <span className="font-mono">{plugin.contentHash.slice(0, 12)}…</span>
                </div>
                <div>
                  <span className="font-medium text-foreground">Capabilities:</span>{" "}
                  {plugin.requiredCapabilities.length > 0
                    ? plugin.requiredCapabilities.join(", ")
                    : "none"}
                </div>
                <div>
                  <span className="font-medium text-foreground">Dependencies:</span>{" "}
                  {plugin.resolvedDependencies.length > 0
                    ? plugin.resolvedDependencies.join(", ")
                    : "none"}
                </div>
              </dl>
              {!plugin.contentVerified && (
                <p className="mt-2 text-xs text-destructive">
                  The plugin tree no longer matches its recorded content hash. It
                  will not load; reinstall it to restore a verified state.
                </p>
              )}
              {plugin.diagnostics.length > 0 && (
                <ul className="mt-2 space-y-1 text-xs text-muted-foreground">
                  {plugin.diagnostics.map((diagnostic, index) => (
                    <li key={`${diagnostic.code}-${index}`}>
                      <span className="font-medium text-foreground">{diagnostic.level}</span> ·{" "}
                      {diagnostic.message}{" "}
                      <span className="font-mono">({diagnostic.path})</span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
            )
          })
        )}
      </CardContent>
    </Card>
  )
}

function WorkersCard({ workers }: { workers: WorkerRecord[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Cpu className="h-4 w-4" /> Compatibility workers
        </CardTitle>
        <CardDescription>
          Vendor workers run in isolated processes. A crash or restart is
          contained and shown here; it never stops the core.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {workers.length === 0 ? (
          <EmptyNote>No compatibility workers are running.</EmptyNote>
        ) : (
          <ul className="space-y-2">
            {workers.map((worker) => {
              const crashed = worker.status === "failed"
              // `restarted` means the worker crashed and was respawned: warn,
              // do not report it as healthy.
              const restarted = worker.status === "restarted"
              const tone = crashed ? "destructive" : restarted ? "warning" : "success"
              return (
                <li
                  key={worker.id}
                  className="flex flex-wrap items-center gap-2 rounded-md border p-3 text-sm"
                >
                  {crashed || restarted ? (
                    <ShieldAlert
                      className={
                        crashed
                          ? "h-4 w-4 text-destructive"
                          : "h-4 w-4 text-amber-600 dark:text-amber-400"
                      }
                    />
                  ) : (
                    <ShieldCheck className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                  )}
                  <span className="font-mono text-xs">{worker.id}</span>
                  <Badge variant={tone}>{worker.status}</Badge>
                  <span className="text-xs text-muted-foreground">
                    attempt {worker.attempt}
                  </span>
                  {worker.code && (
                    <span className="ml-auto font-mono text-xs text-muted-foreground">
                      {worker.code}
                    </span>
                  )}
                </li>
              )
            })}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}

function HookReviewCard({
  hooks,
  pendingKey,
  onReview,
}: {
  hooks: HookRecord[]
  /** `${plugin}/${hook}` whose review mutation is in flight, if any. */
  pendingKey: string | null
  onReview: (hook: HookRecord, approved: boolean) => void
}) {
  const pendingCount = hooks.filter((hook) => !hook.approved).length
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <ShieldCheck className="h-4 w-4" /> Hook review
          {pendingCount > 0 && (
            <Badge variant="warning" className="ml-1">
              {pendingCount} pending
            </Badge>
          )}
        </CardTitle>
        <CardDescription>
          Approve a hook only after reviewing its exact command/arguments.
          Revoking removes the approval; any change to the definition also resets
          it. Pending hooks are not run.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {hooks.length === 0 ? (
          <EmptyNote>No hooks are declared by enabled plugins.</EmptyNote>
        ) : (
          <ul className="space-y-2">
            {hooks.map((hook) => {
              const pending = pendingKey === `${hook.plugin}/${hook.id}`
              return (
              <li
                key={`${hook.plugin}/${hook.id}`}
                className="rounded-md border p-3 text-sm"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{hook.id}</span>
                  <Badge variant="outline">{hook.event}</Badge>
                  <span className="text-xs text-muted-foreground">from {hook.plugin}</span>
                  <Badge variant={hook.approved ? "success" : "warning"}>
                    {hook.approved ? "approved" : "pending review"}
                  </Badge>
                  <div className="ml-auto">
                    {hook.approved ? (
                      <Button
                        size="sm"
                        variant="outline"
                        className="gap-1.5"
                        disabled={pending}
                        aria-busy={pending}
                        onClick={() => onReview(hook, false)}
                      >
                        {pending && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                        Revoke approval
                      </Button>
                    ) : (
                      <Button
                        size="sm"
                        className="gap-1.5"
                        disabled={pending}
                        aria-busy={pending}
                        onClick={() => onReview(hook, true)}
                      >
                        {pending && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                        Approve
                      </Button>
                    )}
                  </div>
                </div>
                <div className="mt-2 space-y-1 text-xs text-muted-foreground">
                  <div className="font-mono">{hook.handler}</div>
                  <div>
                    order {hook.order} · {hook.parallel ? "parallel" : "serial"} ·{" "}
                    trust{" "}
                    <span className="font-mono">{hook.trustHash.slice(0, 12)}…</span>
                  </div>
                  <div>
                    capabilities:{" "}
                    {hook.capabilities.length > 0 ? hook.capabilities.join(", ") : "none"}
                  </div>
                </div>
              </li>
              )
            })}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}

function SkillsCard({ skills }: { skills: SkillRecord[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Skills</CardTitle>
        <CardDescription>
          Instructions parsed from enabled plugin SKILL.md trees. A skill is never
          trusted code; scripts run only through the normal tool path.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {skills.length === 0 ? (
          <EmptyNote>No skills were discovered.</EmptyNote>
        ) : (
          <ul className="space-y-2">
            {skills.map((skill) => (
              <li
                key={`${skill.plugin}/${skill.name}`}
                className="rounded-md border p-3 text-sm"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{skill.name}</span>
                  <span className="text-xs text-muted-foreground">from {skill.plugin}</span>
                </div>
                <p className="mt-1 text-xs text-muted-foreground">{skill.description}</p>
                {skill.allowedTools.length > 0 && (
                  <p className="mt-1 text-xs text-muted-foreground">
                    allowed tools: {skill.allowedTools.join(", ")}
                  </p>
                )}
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}

function McpCard({
  servers,
  policy,
}: {
  servers: McpServerRecord[]
  policy: McpToolPolicyRecord
}) {
  const enabled = policy.enabled
  const policySummary =
    enabled === null
      ? "all non-disabled tools"
      : enabled.length === 0
        ? "no tools (deny-all)"
        : enabled.join(", ")
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Server className="h-4 w-4" /> MCP servers
        </CardTitle>
        <CardDescription>
          MCP servers bundled by enabled plugins, plus the tool policy the core
          applies to their tools.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {servers.length === 0 ? (
          <EmptyNote>No MCP servers are bundled by enabled plugins.</EmptyNote>
        ) : (
          <ul className="space-y-2">
            {servers.map((server) => (
              <li
                key={`${server.plugin}/${server.name}`}
                className="flex flex-wrap items-center gap-2 rounded-md border p-3 text-sm"
              >
                <span className="font-medium">{server.name}</span>
                <Badge variant="outline">{server.transport}</Badge>
                <span className="text-xs text-muted-foreground">from {server.plugin}</span>
                <span className="ml-auto truncate font-mono text-xs text-muted-foreground">
                  {server.endpoint}
                </span>
              </li>
            ))}
          </ul>
        )}
        <div className="rounded-md bg-muted/50 p-3 text-xs text-muted-foreground">
          <div className="font-medium text-foreground">MCP tool policy</div>
          <div className="mt-1">enabled: {policySummary}</div>
          <div>disabled: {policy.disabled.length > 0 ? policy.disabled.join(", ") : "none"}</div>
        </div>
      </CardContent>
    </Card>
  )
}

/** Extracts the canonical error code and a masked detail from a protocol error. */
function protocolFailure(error: unknown): { code: string; detail?: string } | null {
  if (!error || typeof error !== "object" || !("protocol" in error)) return null
  const protocol = (error as {
    protocol?: { coolCode?: unknown; safeDetails?: unknown }
  }).protocol
  if (!protocol || typeof protocol.coolCode !== "string") return null
  let detail: string | undefined
  const safe = protocol.safeDetails
  if (safe && typeof safe === "object" && !Array.isArray(safe)) {
    const value = (safe as Record<string, unknown>).detail
    if (typeof value === "string" && value.trim()) detail = value.trim()
  }
  return { code: protocol.coolCode, detail }
}

/** User-facing copy for an extension admin failure. */
function extensionErrorMessage(error: unknown, fallback: string): string {
  const failure = protocolFailure(error)
  if (!failure) {
    return error instanceof Error && error.message.trim() ? error.message.trim() : fallback
  }
  if (failure.detail) return failure.detail
  switch (failure.code) {
    case "extension_admin_unavailable":
      return "This server has no Rust extension host configured, so extensions cannot be changed here."
    case "extension_state_failed":
      return "Extension state could not be read. Check the plugin store and try again."
    case "extension_mutation_failed":
      return "The change could not be applied. Refresh and try again."
    default:
      return fallback
  }
}
