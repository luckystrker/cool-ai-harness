import { useState } from "react"
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
import {
  MODE_LABELS,
  type PermissionMode,
} from "@/lib/agentConfig"
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
  suggestedModels: string[]
  onModelChange: (model: string) => void
  modelPending?: boolean
  disabled?: boolean
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
  suggestedModels,
  onModelChange,
  modelPending,
  disabled,
}: ComposerToolbarProps) {
  const [browserOpen, setBrowserOpen] = useState(false)

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
          suggestedModels={suggestedModels}
          onChange={onModelChange}
          pending={modelPending}
          disabled={disabled}
        />
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
 * Compact model dropdown for the composer toolbar. Shows provider
 * default_model values plus a "Custom…" entry for arbitrary identifiers.
 */
function ModelPickerInline({
  currentModel,
  suggestedModels,
  onChange,
  pending,
  disabled,
}: {
  currentModel: string
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
      <DropdownMenuContent align="end" className="w-64">
        {currentModel && (
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

        {suggestedModels.length > 0 && (
          <>
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Provider models
            </DropdownMenuLabel>
            {suggestedModels.map((m) => (
              <DropdownMenuItem
                key={m}
                className="font-mono text-xs"
                onSelect={(e) => {
                  e.preventDefault()
                  onChange(m)
                }}
              >
                {m === currentModel ? (
                  <Check className="h-3.5 w-3.5" />
                ) : (
                  <span className="inline-block h-3.5 w-3.5" />
                )}
                {m}
              </DropdownMenuItem>
            ))}
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
