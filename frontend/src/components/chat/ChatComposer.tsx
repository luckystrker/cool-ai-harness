import { useEffect, useRef, useState } from "react"
import { Paperclip, Send, Square, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"

export interface ChatComposerProps {
  onSend: (content: string) => void
  onCancel?: () => void
  onAttach?: (files: File[]) => void
  streaming?: boolean
  disabled?: boolean
  /** Files pending upload (shown as chips above the input). */
  pendingFiles?: File[]
  onRemoveFile?: (index: number) => void
}

/** Auto-growing textarea with send + attach buttons. Enter to send, Shift+Enter for newline. */
export function ChatComposer({
  onSend,
  onCancel,
  onAttach,
  streaming,
  disabled,
  pendingFiles = [],
  onRemoveFile,
}: ChatComposerProps) {
  const [value, setValue] = useState("")
  const ref = useRef<HTMLTextAreaElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)

  // Auto-grow: cap at ~6 lines.
  useEffect(() => {
    const el = ref.current
    if (!el) return
    el.style.height = "auto"
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`
  }, [value])

  const submit = () => {
    const trimmed = value.trim()
    if (!trimmed || disabled || streaming) return
    onSend(trimmed)
    setValue("")
  }

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? [])
    if (files.length && onAttach) onAttach(files)
    // Reset so the same file can be selected again.
    e.target.value = ""
  }

  return (
    <div className="border-t bg-background p-3">
      <div className="mx-auto max-w-3xl">
        {/* Pending file chips */}
        {pendingFiles.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5">
            {pendingFiles.map((f, i) => (
              <span
                key={`${f.name}-${i}`}
                className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-1 text-xs text-muted-foreground"
              >
                <Paperclip className="h-3 w-3" />
                <span className="max-w-[140px] truncate">{f.name}</span>
                <span className="text-muted-foreground/60">
                  ({formatSize(f.size)})
                </span>
                {onRemoveFile && (
                  <button
                    className="ml-0.5 rounded hover:text-foreground"
                    onClick={() => onRemoveFile(i)}
                  >
                    <X className="h-3 w-3" />
                  </button>
                )}
              </span>
            ))}
          </div>
        )}

        <div className="relative flex items-end gap-2">
          {/* Attach button */}
          {onAttach && (
            <>
              <input
                ref={fileInputRef}
                type="file"
                multiple
                className="hidden"
                onChange={handleFileSelect}
              />
              <Button
                size="icon"
                variant="ghost"
                className="h-10 w-10 shrink-0 text-muted-foreground"
                title="Attach files"
                onClick={() => fileInputRef.current?.click()}
                disabled={disabled || streaming}
              >
                <Paperclip className="h-4 w-4" />
              </Button>
            </>
          )}

          <Textarea
            ref={ref}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault()
                submit()
              }
            }}
            placeholder="Message the agent…  (Shift+Enter for newline)"
            rows={1}
            disabled={disabled}
            className="min-h-[40px] resize-none pr-12"
          />
          {streaming ? (
            <Button
              size="icon"
              variant="destructive"
              onClick={onCancel}
              title="Stop"
              className="absolute bottom-1 right-1 h-8 w-8"
            >
              <Square className="h-4 w-4" />
            </Button>
          ) : (
            <Button
              size="icon"
              onClick={submit}
              disabled={!value.trim() || disabled}
              title="Send"
              className="absolute bottom-1 right-1 h-8 w-8"
            >
              <Send className="h-4 w-4" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
