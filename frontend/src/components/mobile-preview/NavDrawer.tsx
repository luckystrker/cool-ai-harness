import { useState } from "react"
import {
  ChevronRight,
  FolderOpen,
  Globe,
  MessageSquare,
  Plus,
  X,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import {
  mockConversations,
  mockProjects,
  mockSections,
} from "./mockData"

/**
 * Slide-over navigation drawer shared by both variants.
 *  - Variant A (drawer nav): full content — new chat, projects,
 *    conversations, and the complete section nav.
 *  - Variant B (tab bar): conversations-only mode; the section nav lives in
 *    the "More" sheet instead.
 *
 * Width: min(320px, 85%) of the screen; backdrop click closes it.
 */
export function NavDrawer({
  open,
  onClose,
  conversationsOnly,
  activeId,
}: {
  open: boolean
  onClose: () => void
  conversationsOnly?: boolean
  activeId: number
}) {
  const [expanded, setExpanded] = useState<Set<string>>(
    () => new Set(mockProjects.map((p) => p.id))
  )

  const toggleProject = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

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
      {/* Panel */}
      <div
        className={cn(
          "absolute inset-y-0 left-0 z-50 flex w-[min(320px,85%)] flex-col border-r bg-background shadow-xl transition-transform duration-200 ease-out",
          open ? "translate-x-0" : "-translate-x-full"
        )}
      >
        {/* Header */}
        <div className="flex h-12 shrink-0 items-center justify-between border-b px-3">
          <div className="flex items-center gap-2">
            <div className="flex h-6 w-6 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <span className="text-xs font-bold">H</span>
            </div>
            <span className="text-sm font-semibold">Harness</span>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="h-10 w-10"
            onClick={onClose}
            title="Close menu"
            aria-label="Close menu"
          >
            <X className="h-4 w-4" />
          </Button>
        </div>

        {/* New chat */}
        <div className="p-2.5">
          <Button className="h-11 w-full justify-start gap-2">
            <Plus className="h-4 w-4" />
            New conversation
          </Button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-2.5 pb-2">
          {/* Projects */}
          <SectionLabel>Projects</SectionLabel>
          <ul className="mb-2 space-y-0.5">
            {mockProjects.map((p) => {
              const chats = mockConversations.filter((c) => c.projectId === p.id)
              const isOpen = expanded.has(p.id)
              return (
                <li key={p.id}>
                  <button
                    className="flex h-11 w-full items-center gap-2 rounded-md px-2 text-sm hover:bg-accent/60"
                    onClick={() => toggleProject(p.id)}
                  >
                    <ChevronRight
                      className={cn(
                        "h-4 w-4 shrink-0 text-muted-foreground transition-transform",
                        isOpen && "rotate-90"
                      )}
                    />
                    {p.kind === "local" ? (
                      <FolderOpen className="h-4 w-4 shrink-0 text-muted-foreground" />
                    ) : (
                      <Globe className="h-4 w-4 shrink-0 text-muted-foreground" />
                    )}
                    <span className="min-w-0 flex-1 truncate text-left font-medium">{p.name}</span>
                    <span className="text-[10px] text-muted-foreground/70">{chats.length}</span>
                  </button>
                  {isOpen &&
                    chats.map((c) => (
                      <ConversationRow key={c.id} title={c.title} active={c.id === activeId} indented />
                    ))}
                </li>
              )
            })}
          </ul>

          {/* Conversations */}
          <SectionLabel>Conversations</SectionLabel>
          <ul className="space-y-0.5">
            {mockConversations
              .filter((c) => c.projectId === null)
              .map((c) => (
                <li key={c.id}>
                  <ConversationRow title={c.title} active={c.id === activeId} />
                </li>
              ))}
          </ul>

          {/* Full section nav — Variant A only */}
          {!conversationsOnly && (
            <>
              <SectionLabel>Sections</SectionLabel>
              <ul className="space-y-0.5">
                {mockSections.map((s) => (
                  <li key={s.key}>
                    <button className="flex h-11 w-full items-center gap-2.5 rounded-md px-2 text-sm text-muted-foreground hover:bg-accent/60 hover:text-foreground">
                      <s.icon className="h-4 w-4 shrink-0" />
                      {s.label}
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      </div>
    </>
  )
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-2 pb-1 pt-2 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
      {children}
    </div>
  )
}

function ConversationRow({
  title,
  active,
  indented,
}: {
  title: string
  active: boolean
  indented?: boolean
}) {
  return (
    <button
      className={cn(
        "flex h-11 w-full items-center gap-2 rounded-md px-2 text-sm",
        indented && "ml-5 w-[calc(100%-1.25rem)] border-l pl-2",
        active ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
      )}
    >
      <MessageSquare className="h-4 w-4 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate text-left">{title}</span>
    </button>
  )
}
