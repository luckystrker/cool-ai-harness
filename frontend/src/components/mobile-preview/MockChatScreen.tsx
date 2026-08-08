import { useEffect, useRef, useState } from "react"
import {
  Check,
  Loader2,
  Menu,
  MoreVertical,
  Plus,
  Send,
  ShieldCheck,
  Sparkles,
  User,
  X,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import {
  ComposerSheet,
  DEFAULT_COMPOSER_SETTINGS,
  type ComposerSettings,
} from "./ComposerSheet"
import { mockMessages, type MockMessage } from "./mockData"

/**
 * Shared mobile chat screen used by both navigation variants. Demonstrates
 * the mobile adaptation rules:
 *  - 48px header with hamburger, truncated title, single overflow action
 *  - touch-scrolling message list (momentum scrolling on iOS)
 *  - composer with >=44px touch targets, auto-grow textarea
 *  - toolbar pickers collapsed into a horizontally scrollable chip row
 *  - flex column layout so the composer follows the virtual keyboard; on a
 *    real Telegram WebApp the layout height should come from
 *    window.visualViewport (or Telegram.WebApp.viewportStableHeight) so
 *    keyboard open/close resizes the flex container instead of overlapping.
 */
export function MockChatScreen({
  title,
  onOpenMenu,
  /** Hide the header hamburger (Variant B uses the tab bar instead). */
  hideMenuButton,
  className,
}: {
  title: string
  onOpenMenu: () => void
  hideMenuButton?: boolean
  className?: string
}) {
  const [draft, setDraft] = useState("")
  const [messages, setMessages] = useState<MockMessage[]>(mockMessages)
  const [responded, setResponded] = useState<"approve" | "deny" | null>(null)
  // Composer settings sheet (opened by the "+" button).
  const [sheetOpen, setSheetOpen] = useState(false)
  const [settings, setSettings] = useState<ComposerSettings>(DEFAULT_COMPOSER_SETTINGS)
  const scrollRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  // Keep the latest message in view (same behavior as the desktop ChatPage).
  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [messages])

  // Auto-grow textarea, capped at ~5 lines to leave room for the keyboard.
  useEffect(() => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = "auto"
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`
  }, [draft])

  const submit = () => {
    const text = draft.trim()
    if (!text) return
    setMessages((prev) => [...prev, { type: "user", text }])
    setDraft("")
  }

  return (
    <div className={cn("relative flex min-h-0 flex-1 flex-col bg-background", className)}>
      {/* --- Mobile header (48px, 3 zones: menu / title / overflow) --- */}
      <header className="flex h-12 shrink-0 items-center gap-1 border-b px-2">
        {!hideMenuButton && (
          <Button
            variant="ghost"
            size="icon"
            className="h-11 w-11 shrink-0"
            title="Open menu"
            onClick={onOpenMenu}
          >
            <Menu className="h-5 w-5" />
          </Button>
        )}
        <span className="min-w-0 flex-1 truncate px-1 text-sm font-semibold">{title}</span>
        <Button
          variant="ghost"
          size="icon"
          className="h-11 w-11 shrink-0 text-muted-foreground"
          title="Conversation options"
        >
          <MoreVertical className="h-5 w-5" />
        </Button>
      </header>

      {/* --- Message list: momentum touch scrolling --- */}
      <div
        ref={scrollRef}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain"
        style={{ WebkitOverflowScrolling: "touch" }}
      >
        <div className="space-y-3 px-3 py-3">
          {messages.map((m, i) => (
            <MockMessageBlock
              key={i}
              msg={m}
              responded={responded}
              onRespond={setResponded}
            />
          ))}
        </div>
      </div>

      {/* --- Composer (pinned; follows the virtual keyboard via flex layout) --- */}
      <div className="shrink-0 border-t bg-background px-2 pb-2 pt-1.5">
        <div className="flex items-end gap-1.5">
          {/* "+" opens the composer settings sheet (mode / model /
              permissions) — 44px touch target. */}
          <Button
            variant="ghost"
            size="icon"
            className="h-11 w-11 shrink-0 text-muted-foreground"
            title="Chat settings"
            onClick={() => setSheetOpen(true)}
          >
            <Plus className="h-5 w-5" />
          </Button>
          <textarea
            ref={textareaRef}
            value={draft}
            rows={1}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              // Enter-to-send is kept but secondary on touch: the send button
              // is the primary affordance (virtual keyboards show their own
              // "return" key, which inserts a newline here).
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault()
                submit()
              }
            }}
            placeholder="Message the agent…"
            className="min-h-[44px] flex-1 resize-none rounded-lg border border-input bg-background px-3 py-2.5 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
          />
          {/* Send — 44px touch target, primary mobile affordance */}
          <Button
            size="icon"
            className="h-11 w-11 shrink-0"
            disabled={!draft.trim()}
            title="Send"
            onClick={submit}
          >
            <Send className="h-5 w-5" />
          </Button>
        </div>
      </div>

      {/* Bottom sheet: permissions, agent mode, model picker, etc. */}
      <ComposerSheet
        open={sheetOpen}
        onClose={() => setSheetOpen(false)}
        settings={settings}
        onChange={setSettings}
      />
    </div>
  )
}

function MockMessageBlock({
  msg,
  responded,
  onRespond,
}: {
  msg: MockMessage
  responded: "approve" | "deny" | null
  onRespond: (r: "approve" | "deny") => void
}) {
  if (msg.type === "user") {
    return (
      <div className="flex justify-end">
        <div className="flex max-w-[85%] items-start gap-2">
          <div className="rounded-2xl rounded-br-md bg-primary px-3 py-2 text-sm text-primary-foreground">
            {msg.text}
          </div>
          <Avatar icon={User} className="bg-muted text-muted-foreground" />
        </div>
      </div>
    )
  }
  if (msg.type === "assistant") {
    return (
      <div className="flex justify-start">
        <div className="flex max-w-[85%] items-start gap-2">
          <Avatar icon={Sparkles} className="bg-violet-500 text-white" />
          <div className="rounded-2xl rounded-bl-md bg-muted px-3 py-2 text-sm">{msg.text}</div>
        </div>
      </div>
    )
  }
  if (msg.type === "tool") {
    return (
      <div className="ml-8 flex items-center gap-2 rounded-lg border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        {msg.status === "running" ? (
          <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" />
        ) : (
          <Check className="h-3.5 w-3.5 shrink-0 text-emerald-600" />
        )}
        <span className="truncate font-mono">{msg.name}</span>
      </div>
    )
  }
  // Approval card — full-width, stacked 44px buttons for thumbs.
  return (
    <div className="rounded-xl border border-amber-300/60 bg-amber-50 p-3 dark:border-amber-500/30 dark:bg-amber-950/20">
      <div className="mb-1 flex items-center gap-2 text-sm font-semibold">
        <ShieldCheck className="h-4 w-4 text-amber-600" />
        {msg.title}
      </div>
      <p className="mb-2 break-words text-xs text-muted-foreground">{msg.detail}</p>
      {responded ? (
        <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
          {responded === "approve" ? (
            <>
              <Check className="h-3.5 w-3.5 text-emerald-600" /> Approved
            </>
          ) : (
            <>
              <X className="h-3.5 w-3.5 text-destructive" /> Denied
            </>
          )}
        </div>
      ) : (
        <div className="flex gap-2">
          <Button
            className="h-11 flex-1"
            onClick={() => onRespond("approve")}
          >
            Approve
          </Button>
          <Button
            variant="outline"
            className="h-11 flex-1"
            onClick={() => onRespond("deny")}
          >
            Deny
          </Button>
        </div>
      )}
    </div>
  )
}

function Avatar({
  icon: Icon,
  className,
}: {
  icon: React.ComponentType<{ className?: string }>
  className?: string
}) {
  return (
    <div
      className={cn(
        "mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full",
        className
      )}
    >
      <Icon className="h-3.5 w-3.5" />
    </div>
  )
}
