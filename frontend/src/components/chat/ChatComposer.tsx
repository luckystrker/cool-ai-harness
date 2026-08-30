import { useEffect, useRef, useState } from "react"
import { FileText, Image as ImageIcon, Paperclip, Send, Square, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"

export interface ChatComposerProps {
  onSend: (content: string) => void
  onCancel?: () => void
  onAttach?: (files: File[]) => void
  streaming?: boolean
  disabled?: boolean
  /** Optional first-use draft. The user can edit it before sending. */
  initialValue?: string
  /** Files pending upload (shown as chips above the input). */
  pendingFiles?: File[]
  onRemoveFile?: (index: number) => void
  /** Optional toolbar rendered below the input (workdir, mode, model pickers). */
  toolbar?: React.ReactNode
  /**
   * Optional leading control rendered instead of the attach button (mobile
   * replaces the paperclip with a "+" that opens the settings sheet).
   */
  leading?: React.ReactNode
}

/** Auto-growing textarea with send + attach buttons. Enter to send, Shift+Enter for newline. */
export function ChatComposer({
  onSend,
  onCancel,
  onAttach,
  streaming,
  disabled,
  initialValue = "",
  pendingFiles = [],
  onRemoveFile,
  toolbar,
  leading,
}: ChatComposerProps) {
  const [value, setValue] = useState(initialValue)
  const [dragging, setDragging] = useState(false)
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
    if ((!trimmed && pendingFiles.length === 0) || disabled || streaming) return
    onSend(trimmed || "Analyze the attached file(s).")
    setValue("")
  }

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? [])
    if (files.length && onAttach) onAttach(files)
    // Reset so the same file can be selected again.
    e.target.value = ""
  }

  return (
    <div
      className={`border-t bg-background p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] ${
        dragging ? "ring-2 ring-inset ring-primary" : ""
      }`}
      onDragEnter={(event) => {
        event.preventDefault()
        if (!disabled && !streaming) setDragging(true)
      }}
      onDragOver={(event) => event.preventDefault()}
      onDragLeave={(event) => {
        if (event.currentTarget === event.target) setDragging(false)
      }}
      onDrop={(event) => {
        event.preventDefault()
        setDragging(false)
        const files = Array.from(event.dataTransfer.files)
        if (files.length && onAttach && !disabled && !streaming) onAttach(files)
      }}
    >
      <div className="mx-auto max-w-3xl">
        {/* Pending file chips */}
        {pendingFiles.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-1.5">
            {pendingFiles.map((f, i) => (
              <span
                key={`${f.name}-${i}`}
                className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-1 text-xs text-muted-foreground"
              >
                <FilePreviewIcon file={f} />
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
          {/* Leading control: custom (mobile "+") or the attach button */}
          {leading ??
            (onAttach && (
              <>
                <input
                  ref={fileInputRef}
                  type="file"
                  multiple
                  accept="image/png,image/jpeg,image/webp,image/gif,application/pdf,text/*,.docx"
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
            ))}

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
            className="min-h-11 resize-none pr-14 text-base md:text-sm"
          />
          {streaming ? (
            <Button
              size="icon"
              variant="destructive"
              onClick={onCancel}
              title="Stop"
              className="absolute bottom-0 right-0 h-11 w-11"
            >
              <Square className="h-4 w-4" />
            </Button>
          ) : (
            <Button
              size="icon"
              onClick={submit}
              disabled={(!value.trim() && pendingFiles.length === 0) || disabled}
              title="Send"
              className="absolute bottom-0 right-0 h-11 w-11"
            >
              <Send className="h-4 w-4" />
            </Button>
          )}
        </div>

        {toolbar}
      </div>
    </div>
  )
}

function FilePreviewIcon({ file }: { file: File }) {
  const [url, setUrl] = useState<string | null>(null)

  useEffect(() => {
    if (!file.type.startsWith("image/")) return
    const objectUrl = URL.createObjectURL(file)
    setUrl(objectUrl)
    return () => URL.revokeObjectURL(objectUrl)
  }, [file])

  if (url) {
    return <img src={url} alt="" className="h-8 w-8 rounded object-cover" />
  }
  return file.type.startsWith("image/") ? (
    <ImageIcon className="h-3 w-3" />
  ) : (
    <FileText className="h-3 w-3" />
  )
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
