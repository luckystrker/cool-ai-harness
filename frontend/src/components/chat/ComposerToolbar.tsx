import { useMemo, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import {
  Check,
  ChevronDown,
  Cpu,
  FolderOpen,
  GitBranch,
  Pencil,
  ShieldCheck,
} from "lucide-react"
import { workspaceApi } from "@/api/workspace"
import type { ModelInfo } from "@/api/types"
import {
  MODE_LABELS,
  type PermissionMode,
} from "@/lib/agentConfig"
import { formatContextWindow, hasModelMeta, contextUsagePct } from "@/lib/modelFormat"
import { DirectoryBrowserDialog } from "@/components/chat/DirectoryBrowserDialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"

export interface ComposerToolbarProps {
  workingDirectory: string | null
  onWorkingDirectoryChange: (dir: string) => void
  mode: PermissionMode | null
  onModeChange: (mode: PermissionMode) => void
  currentModel: string
  /**
   * Models served by the active provider (live /models response), used both to
   * populate the picker and to resolve the current model's context window.
   * Falls back to provider.default_model strings when the live list is empty.
   */
  modelOptions: ModelInfo[]
  /** Additional model id hints (e.g. other providers' defaults) merged in. */
  suggestedModels: string[]
  /** Tokens of conversation context already consumed (last assistant prompt_tokens). */
  usedContextTokens?: number | null
  onModelChange: (model: string) => void
  modelPending?: boolean
  disabled?: boolean
  /** Plan Mode toggle (Фаза 2 §1). */
  planMode?: boolean
  onPlanModeChange?: (enabled: boolean) => void
}

/** Last path segment — used as the compact display name for a directory. */
function dirLabel(path: string | null | undefined): string {
  if (!path) return "Workspace"
  const normalized = path.replace(/[\\/]+$/, "")
  const seg = normalized.split(/[\\/]/).pop()
  return seg || normalized
}

/**
 * Toolbar rendered inside the chat composer (below the textarea), following
 * the familiar AI-agent layout: working-directory picker with recent
 * projects + folder browser, current git branch badge, agent mode selector,
 * and model selector.
 */
export function ComposerToolbar({
  workingDirectory,
  onWorkingDirectoryChange,
  mode,
  onModeChange,
  currentModel,
  modelOptions,
  suggestedModels,
  usedContextTokens,
  onModelChange,
  modelPending,
  disabled,
  planMode,
  onPlanModeChange,
}: ComposerToolbarProps) {
  const [browserOpen, setBrowserOpen] = useState(false)

  // Resolve the active model's context window from the live model list.
  const currentModelContext =
    modelOptions.find((m) => m.id === currentModel)?.context_window ?? null
  const usedPct = contextUsagePct(usedContextTokens, currentModelContext)

  // Recent projects for the working-directory dropdown.
  const { data: recentData } = useQuery({
    queryKey: ["workspace-recent"],
    queryFn: workspaceApi.recent,
  })

  // Git branch for the active working directory.
  const { data: gitInfo } = useQuery({
    queryKey: ["git-info", workingDirectory ?? recentData?.default ?? ""],
    queryFn: () =>
      workspaceApi.gitInfo(workingDirectory || recentData?.default || ""),
    enabled: Boolean(workingDirectory || recentData?.default),
  })

  const modeEntry = MODE_LABELS.find((m) => m.mode === mode)

  return (
    <>
      <div className="flex items-center gap-1 pt-1.5">
        {/* --- Working directory picker --- */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              disabled={disabled}
              className="h-7 max-w-[200px] gap-1.5 px-2 text-xs font-normal text-muted-foreground"
              title={workingDirectory ?? recentData?.default ?? "Default workspace"}
            >
              <FolderOpen className="h-3.5 w-3.5 shrink-0" />
              <span className="truncate">{dirLabel(workingDirectory ?? recentData?.default)}</span>
              <ChevronDown className="h-3 w-3 shrink-0" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-72">
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Recent projects
            </DropdownMenuLabel>
            {(recentData?.recent ?? []).map((dir) => (
              <DropdownMenuItem
                key={dir}
                className="gap-2 text-xs"
                onSelect={() => onWorkingDirectoryChange(dir)}
              >
                {dir === workingDirectory ? (
                  <Check className="h-3.5 w-3.5 shrink-0" />
                ) : (
                  <span className="inline-block h-3.5 w-3.5 shrink-0" />
                )}
                <span className="truncate font-mono" title={dir}>
                  {dir}
                </span>
              </DropdownMenuItem>
            ))}
            {(!recentData?.recent.length) && (
              <DropdownMenuItem disabled className="text-xs">
                No recent projects
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem
              className="gap-2 text-xs"
              onSelect={() => setBrowserOpen(true)}
            >
              <FolderOpen className="h-3.5 w-3.5" />
              Browse…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>

        {/* --- Git branch badge --- */}
        {gitInfo?.is_git && gitInfo.branch && (
          <span
            className="inline-flex h-7 items-center gap-1 rounded-md bg-muted px-2 text-xs text-muted-foreground"
            title={`Git branch: ${gitInfo.branch}`}
          >
            <GitBranch className="h-3 w-3" />
            {gitInfo.branch}
          </span>
        )}

        {/* --- Agent mode: Build / Plan (Фаза 2 §1) --- */}
        {onPlanModeChange && (
          <div className="flex h-7 items-center rounded-md border text-xs">
            <button
              disabled={disabled}
              className={cn(
                "h-full rounded-l-md px-2 transition-colors",
                !planMode
                  ? "bg-primary text-primary-foreground"
                  : "text-muted-foreground hover:text-foreground"
              )}
              title="Build mode: agent executes tasks directly"
              onClick={() => onPlanModeChange(false)}
            >
              Build
            </button>
            <button
              disabled={disabled}
              className={cn(
                "h-full rounded-r-md px-2 transition-colors",
                planMode
                  ? "bg-primary text-primary-foreground"
                  : "text-muted-foreground hover:text-foreground"
              )}
              title="Plan mode: agent researches first, then produces a structured plan"
              onClick={() => onPlanModeChange(true)}
            >
              Plan
            </button>
          </div>
        )}

        <div className="flex-1" />

        {/* --- Agent mode picker --- */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              disabled={disabled}
              className="h-7 gap-1.5 px-2 text-xs font-normal text-muted-foreground"
              title={modeEntry ? `${modeEntry.label} — ${modeEntry.hint}` : "Agent mode"}
            >
              <ShieldCheck className="h-3.5 w-3.5" />
              <span>{modeEntry?.label ?? "Mode"}</span>
              <ChevronDown className="h-3 w-3" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-52">
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Agent mode
            </DropdownMenuLabel>
            {MODE_LABELS.map(({ mode: m, label, hint }) => (
              <DropdownMenuItem
                key={m}
                className="gap-2 text-xs"
                onSelect={() => onModeChange(m)}
              >
                {mode === m ? (
                  <Check className="h-3.5 w-3.5 shrink-0" />
                ) : (
                  <span className="inline-block h-3.5 w-3.5 shrink-0" />
                )}
                <span>
                  <span className="block font-medium">{label}</span>
                  <span className="block text-[10px] text-muted-foreground">{hint}</span>
                </span>
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>

        {/* --- Model picker --- */}
        <ModelPickerInline
          currentModel={currentModel}
          modelOptions={modelOptions}
          suggestedModels={suggestedModels}
          onChange={onModelChange}
          pending={modelPending}
          disabled={disabled}
        />

        {/* --- Context window badge for the current model --- */}
        {currentModel && (
          <span
            className={cn(
              "inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-xs text-muted-foreground",
              usedPct >= 80 ? "bg-destructive/10 text-destructive" : "bg-muted"
            )}
            title={
              usedContextTokens != null && currentModelContext
                ? `Context: ${usedContextTokens.toLocaleString()} / ${currentModelContext.toLocaleString()} tokens (${usedPct}%)`
                : `Context window: ${formatContextWindow(currentModelContext)} tokens`
            }
          >
            <Cpu className="h-3 w-3 shrink-0" />
            {usedContextTokens != null && currentModelContext ? (
              <span className="flex items-center gap-1">
                <span>
                  {formatContextWindow(usedContextTokens)} / {formatContextWindow(currentModelContext)}
                </span>
                <span className="relative h-1.5 w-10 overflow-hidden rounded-full bg-background/60">
                  <span
                    className={cn(
                      "absolute inset-y-0 left-0 rounded-full",
                      usedPct >= 80 ? "bg-destructive" : "bg-primary"
                    )}
                    style={{ width: `${usedPct}%` }}
                  />
                </span>
              </span>
            ) : usedContextTokens != null ? (
              <span>{formatContextWindow(usedContextTokens)} used</span>
            ) : (
              <span>ctx {formatContextWindow(currentModelContext)}</span>
            )}
          </span>
        )}
      </div>

      <DirectoryBrowserDialog
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        initialPath={workingDirectory ?? recentData?.default}
        onSelect={onWorkingDirectoryChange}
      />
    </>
  )
}

/**
 * Compact model dropdown for the composer toolbar. Shows models from the
 * active provider's live /models list (with context window + price) plus
 * fallback default_model strings, and a "Custom…" entry for arbitrary ids.
 */
function ModelPickerInline({
  currentModel,
  modelOptions,
  suggestedModels,
  onChange,
  pending,
  disabled,
}: {
  currentModel: string
  modelOptions: ModelInfo[]
  suggestedModels: string[]
  onChange: (model: string) => void
  pending?: boolean
  disabled?: boolean
}) {
  const [customOpen, setCustomOpen] = useState(false)
  const [customValue, setCustomValue] = useState("")

  const submitCustom = () => {
    const v = customValue.trim()
    if (!v) return
    onChange(v)
    setCustomOpen(false)
    setCustomValue("")
  }

  // The picker shows ONLY the models the user marked as available in chat
  // (suggestedModels = union of providers' chat_models). Live model metadata
  // (context window / price) from /models is resolved by id lookup and shown
  // where present; the full live list is never rendered directly.
  const metaById = useMemo(() => {
    const m = new Map<string, ModelInfo>()
    for (const opt of modelOptions) m.set(opt.id, opt)
    return m
  }, [modelOptions])

  const hasAnyOptions = suggestedModels.length > 0

  return (
    <DropdownMenu onOpenChange={(open) => { if (!open) setCustomOpen(false) }}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          disabled={disabled}
          className="h-7 max-w-[180px] gap-1.5 px-2 text-xs font-normal text-muted-foreground"
          title="Change model"
        >
          <Cpu className="h-3.5 w-3.5 shrink-0" />
          {pending ? (
            <span>saving…</span>
          ) : (
            <span className={cn("truncate font-mono", !currentModel && "font-sans")}>
              {currentModel || "Set model"}
            </span>
          )}
          <ChevronDown className="h-3 w-3 shrink-0" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-72">
        {currentModel && !suggestedModels.includes(currentModel) && (
          <>
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Current
            </DropdownMenuLabel>
            <DropdownMenuItem
              className="font-mono text-xs"
              onSelect={(e) => e.preventDefault()}
            >
              <Check className="h-3.5 w-3.5" /> {currentModel}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
          </>
        )}

        {hasAnyOptions && (
          <>
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Available models
            </DropdownMenuLabel>
            {suggestedModels.map((id) => {
              const meta = metaById.get(id)
              return (
                <DropdownMenuItem
                  key={id}
                  className="flex items-start gap-2 py-1.5"
                  onSelect={(e) => {
                    e.preventDefault()
                    onChange(id)
                  }}
                >
                  {id === currentModel ? (
                    <Check className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  ) : (
                    <span className="mt-0.5 inline-block h-3.5 w-3.5 shrink-0" />
                  )}
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-mono text-xs">{id}</span>
                    {meta && hasModelMeta(meta) && (
                      <span className="mt-0.5 block text-[10px] text-muted-foreground">
                        ctx {formatContextWindow(meta.context_window)}
                      </span>
                    )}
                  </span>
                </DropdownMenuItem>
              )
            })}
            <DropdownMenuSeparator />
          </>
        )}

        {customOpen ? (
          <form
            className="flex items-center gap-1 p-1"
            onSubmit={(e) => {
              e.preventDefault()
              submitCustom()
            }}
          >
            <Input
              autoFocus
              placeholder="model name"
              value={customValue}
              onChange={(e) => setCustomValue(e.target.value)}
              className="h-8 font-mono text-xs"
            />
            <Button type="submit" size="sm" className="h-8 px-2">
              Set
            </Button>
          </form>
        ) : (
          <DropdownMenuItem
            className="text-xs"
            onSelect={(e) => {
              e.preventDefault()
              setCustomOpen(true)
            }}
          >
            <Pencil className="h-3.5 w-3.5" /> Custom model…
          </DropdownMenuItem>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
