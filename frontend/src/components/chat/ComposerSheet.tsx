import { useMemo, useRef, useState } from "react"
import * as DialogPrimitive from "@radix-ui/react-dialog"
import { Check, FolderOpen, Paperclip, X } from "lucide-react"
import type { ModelInfo } from "@/api/types"
import { DirectoryBrowserDialog } from "@/components/chat/DirectoryBrowserDialog"
import { Button } from "@/components/ui/button"
import { MODE_LABELS, type PermissionMode } from "@/lib/agentConfig"
import { formatContextWindow, hasModelMeta } from "@/lib/modelFormat"
import { cn } from "@/lib/utils"

export interface ComposerSheetProps {
  open: boolean
  onClose: () => void
  workingDirectory: string | null
  onWorkingDirectoryChange: (dir: string) => void
  mode: PermissionMode | null
  onModeChange: (mode: PermissionMode) => void
  currentModel: string
  modelOptions: ModelInfo[]
  suggestedModels: string[]
  onModelChange: (model: string) => void
  planMode: boolean
  onPlanModeChange: (v: boolean) => void
  pendingFiles: File[]
  onAttach: (files: File[]) => void
  onRemoveFile: (index: number) => void
}

/**
 * Mobile bottom sheet with everything the desktop ComposerToolbar holds —
 * attachments, workspace, agent mode (permissions), model and plan mode —
 * collapsed behind the composer's "+" button so the 320–768px layout stays
 * clean. Mirrors the approved Variant B mock.
 */
export function ComposerSheet({
  open,
  onClose,
  workingDirectory,
  onWorkingDirectoryChange,
  mode,
  onModeChange,
  currentModel,
  modelOptions,
  suggestedModels,
  onModelChange,
  planMode,
  onPlanModeChange,
  pendingFiles,
  onAttach,
  onRemoveFile,
}: ComposerSheetProps) {
  const fileInputRef = useRef<HTMLInputElement>(null)
  const returnFocusRef = useRef<HTMLElement | null>(null)
  const [browserOpen, setBrowserOpen] = useState(false)
  const [customOpen, setCustomOpen] = useState(false)
  const [customValue, setCustomValue] = useState("")

  const metaById = useMemo(() => {
    const m = new Map<string, ModelInfo>()
    for (const opt of modelOptions) m.set(opt.id, opt)
    return m
  }, [modelOptions])

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? [])
    if (files.length) onAttach(files)
    e.target.value = ""
  }

  const submitCustom = () => {
    const v = customValue.trim()
    if (!v) return
    onModelChange(v)
    setCustomOpen(false)
    setCustomValue("")
  }

  return (
    <>
      <DialogPrimitive.Root
        open={open}
        onOpenChange={(nextOpen) => !nextOpen && onClose()}
      >
        <DialogPrimitive.Portal>
          <DialogPrimitive.Overlay className="motion-opacity fixed inset-0 z-40 bg-black/40 data-[state=closed]:opacity-0 data-[state=open]:opacity-100" />
          <DialogPrimitive.Content
            className="motion-spatial fixed inset-x-0 bottom-0 z-50 flex max-h-[78dvh] translate-y-full flex-col rounded-t-2xl border-t bg-background shadow-lg outline-none transition-transform duration-200 data-[state=open]:translate-y-0"
            onOpenAutoFocus={() => {
              returnFocusRef.current = document.activeElement as HTMLElement | null
            }}
            onCloseAutoFocus={(event) => {
              event.preventDefault()
              returnFocusRef.current?.focus()
            }}
          >
            <div className="mx-auto mt-2 h-1 w-10 shrink-0 rounded-full bg-muted-foreground/30" />
            <div className="flex h-12 shrink-0 items-center justify-between px-4">
              <DialogPrimitive.Title className="text-sm font-semibold">
                Chat settings
              </DialogPrimitive.Title>
              <DialogPrimitive.Description className="sr-only">
                Configure attachments, workspace, agent mode, model, and run mode.
              </DialogPrimitive.Description>
              <DialogPrimitive.Close asChild>
                <button
                  className="flex h-11 w-11 items-center justify-center rounded-md text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  aria-label="Close chat settings"
                >
                  <X className="h-5 w-5" />
                </button>
              </DialogPrimitive.Close>
            </div>

            <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-4 pb-6">
          {/* --- Attachments --- */}
          <section>
            <SheetLabel>Attachments</SheetLabel>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={handleFileSelect}
            />
            <button
              className="flex min-h-12 w-full items-center justify-center gap-2 rounded-lg border border-dashed px-3 py-2.5 text-sm text-muted-foreground hover:bg-accent/50 hover:text-foreground"
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip className="h-4 w-4" />
              Attach file
            </button>
            {pendingFiles.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1.5">
                {pendingFiles.map((f, i) => (
                  <span
                    key={`${f.name}-${i}`}
                    className="inline-flex items-center gap-1.5 rounded-md bg-muted px-2 py-1.5 text-xs"
                  >
                    <Paperclip className="h-3 w-3 text-muted-foreground" />
                    <span className="max-w-[140px] truncate">{f.name}</span>
                    <button
                      className="rounded hover:text-destructive"
                      onClick={() => onRemoveFile(i)}
                      aria-label={`Remove ${f.name}`}
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </span>
                ))}
              </div>
            )}
          </section>

          {/* --- Workspace --- */}
          <section>
            <SheetLabel>Workspace</SheetLabel>
            <button
              className="flex min-h-12 w-full items-center gap-2 rounded-lg border px-3 py-2.5 text-sm hover:bg-accent/50"
              onClick={() => setBrowserOpen(true)}
              title={workingDirectory ?? "Choose a working directory"}
            >
              <FolderOpen className="h-4 w-4 shrink-0 text-muted-foreground" />
              <span className="truncate font-mono text-xs">
                {workingDirectory ? dirLabel(workingDirectory) : "Not set — browse…"}
              </span>
            </button>
          </section>

          {/* --- Agent mode (permissions) --- */}
          <section>
            <SheetLabel>Agent mode</SheetLabel>
            <div className="space-y-1.5">
              {MODE_LABELS.map(({ mode: m, label, hint }) => (
                <button
                  key={m}
                  className={cn(
                    "flex min-h-12 w-full items-center gap-3 rounded-lg border px-3 py-2.5 text-left",
                    mode === m ? "border-primary/50 bg-accent" : "hover:bg-accent/50"
                  )}
                  onClick={() => onModeChange(m)}
                >
                  <span className="min-w-0 flex-1">
                    <span className="block text-sm font-medium">{label}</span>
                    <span className="block text-xs text-muted-foreground">{hint}</span>
                  </span>
                  {mode === m && <Check className="h-4 w-4 shrink-0 text-primary" />}
                </button>
              ))}
            </div>
          </section>

          {/* --- Model --- */}
          <section>
            <SheetLabel>Model</SheetLabel>
            <div className="space-y-1.5">
              {currentModel && !suggestedModels.includes(currentModel) && (
                <ModelRow
                  id={currentModel}
                  meta={metaById.get(currentModel)}
                  active
                  onSelect={() => {}}
                />
              )}
              {suggestedModels.map((id) => (
                <ModelRow
                  key={id}
                  id={id}
                  meta={metaById.get(id)}
                  active={id === currentModel}
                  onSelect={() => onModelChange(id)}
                />
              ))}
              {customOpen ? (
                <form
                  className="flex items-center gap-1.5"
                  onSubmit={(e) => {
                    e.preventDefault()
                    submitCustom()
                  }}
                >
                  <input
                    autoFocus
                    aria-label="Custom model name"
                    placeholder="model name"
                    value={customValue}
                    onChange={(e) => setCustomValue(e.target.value)}
                    className="h-11 min-w-0 flex-1 rounded-lg border bg-background px-3 font-mono text-xs"
                  />
                  <Button type="submit" size="sm" className="h-11 px-3">
                    Set
                  </Button>
                </form>
              ) : (
                <button
                  className="flex min-h-11 w-full items-center rounded-lg px-3 text-xs text-muted-foreground hover:bg-accent/50 hover:text-foreground"
                  onClick={() => setCustomOpen(true)}
                >
                  Custom model…
                </button>
              )}
            </div>
          </section>

          {/* --- Plan mode --- */}
          <section>
            <SheetLabel>Run mode</SheetLabel>
            <div className="flex h-12 items-center rounded-lg border text-sm">
              <button
                className={cn(
                  "h-full flex-1 rounded-l-lg transition-colors",
                  !planMode
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:text-foreground"
                )}
                onClick={() => onPlanModeChange(false)}
                aria-pressed={!planMode}
              >
                Build
              </button>
              <button
                className={cn(
                  "h-full flex-1 rounded-r-lg transition-colors",
                  planMode
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:text-foreground"
                )}
                onClick={() => onPlanModeChange(true)}
                aria-pressed={planMode}
              >
                Plan
              </button>
            </div>
          </section>
            </div>

            <div className="shrink-0 border-t p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
              <DialogPrimitive.Close asChild>
                <Button className="w-full">Done</Button>
              </DialogPrimitive.Close>
            </div>
          </DialogPrimitive.Content>
        </DialogPrimitive.Portal>
      </DialogPrimitive.Root>

      <DirectoryBrowserDialog
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        initialPath={workingDirectory ?? undefined}
        onSelect={onWorkingDirectoryChange}
      />
    </>
  )
}

function ModelRow({
  id,
  meta,
  active,
  onSelect,
}: {
  id: string
  meta?: ModelInfo
  active: boolean
  onSelect: () => void
}) {
  return (
    <button
      className={cn(
        "flex min-h-12 w-full items-center gap-3 rounded-lg border px-3 py-2.5 text-left",
        active ? "border-primary/50 bg-accent" : "hover:bg-accent/50"
      )}
      onClick={onSelect}
      aria-pressed={active}
    >
      <span className="min-w-0 flex-1">
        <span className="block truncate font-mono text-xs">{id}</span>
        {meta && hasModelMeta(meta) && (
          <span className="block text-[10px] text-muted-foreground">
            ctx {formatContextWindow(meta.context_window)}
          </span>
        )}
      </span>
      {active && <Check className="h-4 w-4 shrink-0 text-primary" />}
    </button>
  )
}

function SheetLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
      {children}
    </div>
  )
}

/** Last path segment of a directory, for compact display. */
function dirLabel(dir: string): string {
  const parts = dir.replace(/\\/g, "/").split("/").filter(Boolean)
  return parts[parts.length - 1] || dir
}
