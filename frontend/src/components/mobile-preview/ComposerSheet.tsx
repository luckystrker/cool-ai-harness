import { useState } from "react"
import { Check, Paperclip, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

/** Composer settings shown in the bottom sheet (mock values). */
export interface ComposerSettings {
  mode: "ask" | "allow_edits" | "allow"
  model: string
  planMode: boolean
  /** Capability toggles (mirrors the backend capability policy names). */
  capabilities: Record<string, boolean>
}

export const DEFAULT_COMPOSER_SETTINGS: ComposerSettings = {
  mode: "allow_edits",
  model: "gpt-5-mini",
  planMode: false,
  capabilities: {
    read: true,
    write: true,
    execute: false,
    network: true,
    git: true,
    send_external: false,
  },
}

/** Mode presets — same labels as the desktop composer (lib/agentConfig). */
const MODES: { key: ComposerSettings["mode"]; label: string; hint: string }[] = [
  { key: "ask", label: "Always ask", hint: "Confirm every tool call" },
  { key: "allow_edits", label: "Allow edits", hint: "Free files; confirm code" },
  { key: "allow", label: "Always allow", hint: "Run everything without asking" },
]

const MODELS = [
  { id: "gpt-5-mini", ctx: "128k" },
  { id: "gpt-5", ctx: "256k" },
  { id: "claude-sonnet-4.5", ctx: "200k" },
  { id: "llama-3.1-70b", ctx: "128k" },
]

const CAPABILITIES: { key: string; label: string; hint: string }[] = [
  { key: "read", label: "Read files", hint: "read_file, list_files" },
  { key: "write", label: "Write files", hint: "write_file, artifacts" },
  { key: "execute", label: "Execute code", hint: "python_execute, bash" },
  { key: "network", label: "Network", hint: "web_search, web_fetch" },
  { key: "git", label: "Git / GitHub", hint: "git_*, github_*" },
  { key: "send_external", label: "External send", hint: "Telegram, webhooks" },
]

/**
 * Bottom sheet opened by the "+" button in the mobile composer. Slides up
 * from the bottom edge (Telegram-friendly pattern), holds per-chat agent
 * settings: permission mode, model picker, plan mode, capability toggles.
 */
export function ComposerSheet({
  open,
  onClose,
  settings,
  onChange,
}: {
  open: boolean
  onClose: () => void
  settings: ComposerSettings
  onChange: (next: ComposerSettings) => void
}) {
  // Mock attachments: tapping "Attach file" adds a fake file chip.
  const [attachments, setAttachments] = useState<string[]>([])
  const MOCK_FILES = ["report.pdf", "screenshot.png", "trace.log"]
  const addMockFile = () =>
    setAttachments((prev) =>
      prev.length >= MOCK_FILES.length
        ? prev
        : [...prev, MOCK_FILES[prev.length]]
    )

  return (
    <>
      {/* Backdrop */}
      <div
        className={cn(
          "absolute inset-0 z-40 bg-black/40 transition-opacity duration-200",
          open ? "opacity-100" : "pointer-events-none opacity-0"
        )}
        onClick={onClose}
      />
      {/* Sheet panel — slides up from the bottom */}
      <div
        className={cn(
          "absolute inset-x-0 bottom-0 z-50 flex max-h-[78%] flex-col rounded-t-2xl border-t bg-background shadow-xl transition-transform duration-200 ease-out",
          open ? "translate-y-0" : "translate-y-full"
        )}
      >
        {/* Drag handle */}
        <div className="flex justify-center pt-2">
          <div className="h-1 w-10 rounded-full bg-muted-foreground/30" />
        </div>

        <div className="flex h-12 shrink-0 items-center justify-between px-4">
          <span className="text-sm font-semibold">Chat settings</span>
          <Button
            variant="ghost"
            size="icon"
            className="h-10 w-10"
            onClick={onClose}
            title="Close"
            aria-label="Close chat settings"
          >
            <X className="h-4 w-4" />
          </Button>
        </div>

        <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-4 pb-6">
          {/* --- Attachments --- */}
          <section>
            <SheetLabel>Attachments</SheetLabel>
            <button
              className="flex min-h-12 w-full items-center justify-center gap-2 rounded-lg border border-dashed px-3 py-2.5 text-sm text-muted-foreground hover:bg-accent/50 hover:text-foreground"
              onClick={addMockFile}
            >
              <Paperclip className="h-4 w-4" />
              Attach file
            </button>
            {attachments.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1.5">
                {attachments.map((name) => (
                  <span
                    key={name}
                    className="inline-flex items-center gap-1.5 rounded-md bg-muted px-2 py-1.5 text-xs"
                  >
                    <Paperclip className="h-3 w-3 text-muted-foreground" />
                    {name}
                    <button
                      className="rounded hover:text-destructive"
                      onClick={() =>
                        setAttachments((prev) => prev.filter((f) => f !== name))
                      }
                      title="Remove"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </span>
                ))}
              </div>
            )}
          </section>

          {/* --- Agent mode --- */}
          <section>
            <SheetLabel>Agent mode</SheetLabel>
            <div className="space-y-1.5">
              {MODES.map((m) => (
                <button
                  key={m.key}
                  className={cn(
                    "flex min-h-12 w-full items-center gap-3 rounded-lg border px-3 py-2 text-left",
                    settings.mode === m.key
                      ? "border-primary/40 bg-accent"
                      : "hover:bg-accent/50"
                  )}
                  onClick={() => onChange({ ...settings, mode: m.key })}
                >
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium">{m.label}</div>
                    <div className="text-xs text-muted-foreground">{m.hint}</div>
                  </div>
                  {settings.mode === m.key && <Check className="h-4 w-4 shrink-0 text-primary" />}
                </button>
              ))}
            </div>
          </section>

          {/* --- Model --- */}
          <section>
            <SheetLabel>Model</SheetLabel>
            <div className="space-y-1.5">
              {MODELS.map((m) => (
                <button
                  key={m.id}
                  className={cn(
                    "flex min-h-12 w-full items-center gap-3 rounded-lg border px-3 py-2 text-left",
                    settings.model === m.id
                      ? "border-primary/40 bg-accent"
                      : "hover:bg-accent/50"
                  )}
                  onClick={() => onChange({ ...settings, model: m.id })}
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-sm">{m.id}</span>
                  <span className="text-xs text-muted-foreground">{m.ctx} ctx</span>
                  {settings.model === m.id && <Check className="h-4 w-4 shrink-0 text-primary" />}
                </button>
              ))}
            </div>
          </section>

          {/* --- Plan mode --- */}
          <section>
            <ToggleRow
              label="Plan mode"
              hint="Draft a plan first; execute after approval"
              checked={settings.planMode}
              onToggle={() => onChange({ ...settings, planMode: !settings.planMode })}
            />
          </section>

          {/* --- Permissions / capabilities --- */}
          <section>
            <SheetLabel>Permissions</SheetLabel>
            <div className="space-y-1.5">
              {CAPABILITIES.map((c) => (
                <ToggleRow
                  key={c.key}
                  label={c.label}
                  hint={c.hint}
                  checked={settings.capabilities[c.key] ?? false}
                  onToggle={() =>
                    onChange({
                      ...settings,
                      capabilities: {
                        ...settings.capabilities,
                        [c.key]: !settings.capabilities[c.key],
                      },
                    })
                  }
                />
              ))}
            </div>
          </section>

          <Button className="h-12 w-full" onClick={onClose}>
            Done
          </Button>
        </div>
      </div>
    </>
  )
}

function SheetLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
      {children}
    </div>
  )
}

/** 48px row with a switch-style toggle — thumb-friendly. */
function ToggleRow({
  label,
  hint,
  checked,
  onToggle,
}: {
  label: string
  hint: string
  checked: boolean
  onToggle: () => void
}) {
  return (
    <button
      className="flex min-h-12 w-full items-center gap-3 rounded-lg px-1 py-2 text-left"
      onClick={onToggle}
      role="switch"
      aria-checked={checked}
    >
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{label}</div>
        <div className="truncate text-xs text-muted-foreground">{hint}</div>
      </div>
      {/* Switch */}
      <span
        className={cn(
          "relative h-6 w-11 shrink-0 rounded-full transition-colors",
          checked ? "bg-primary" : "bg-muted-foreground/25"
        )}
      >
        <span
          className={cn(
            "absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-all",
            checked ? "left-[22px]" : "left-0.5"
          )}
        />
      </span>
    </button>
  )
}
