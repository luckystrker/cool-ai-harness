import { useEffect, useRef, useState } from "react"
import { Send, Square } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"

export interface ChatComposerProps {
  onSend: (content: string) => void
  onCancel?: () => void
  streaming?: boolean
  disabled?: boolean
}

/** Auto-growing textarea with a send button. Enter to send, Shift+Enter for newline. */
export function ChatComposer({ onSend, onCancel, streaming, disabled }: ChatComposerProps) {
  const [value, setValue] = useState("")
  const ref = useRef<HTMLTextAreaElement>(null)

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

  return (
    <div className="border-t bg-background p-3">
      <div className="relative mx-auto flex max-w-3xl items-end gap-2">
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
  )
}
