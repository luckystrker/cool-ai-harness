import { useState } from "react"
import { BarChart3, CalendarClock, CheckCircle2, MessageSquare, MoreHorizontal } from "lucide-react"
import { cn } from "@/lib/utils"
import { MockChatScreen } from "./MockChatScreen"
import { NavDrawer } from "./NavDrawer"
import { mockSections } from "./mockData"

type TabKey = "chat" | "tasks" | "analytics" | "more"

const TABS: { key: TabKey; label: string; icon: React.ComponentType<{ className?: string }> }[] = [
  { key: "chat", label: "Chat", icon: MessageSquare },
  { key: "tasks", label: "Tasks", icon: CalendarClock },
  { key: "analytics", label: "Analytics", icon: BarChart3 },
  { key: "more", label: "More", icon: MoreHorizontal },
]

/**
 * Variant B — persistent bottom tab bar.
 * Primary destinations are one tap away; the hamburger opens a
 * conversations-only drawer, and "More" lists the remaining sections.
 * Trades screen space (56px permanent bar) for faster navigation.
 */
export function TabBarVariant() {
  const [tab, setTab] = useState<TabKey>("chat")
  const [drawerOpen, setDrawerOpen] = useState(false)

  return (
    <div className="relative flex h-full min-h-0 flex-col overflow-hidden">
      {/* Active tab content */}
      <div className="flex min-h-0 flex-1 flex-col">
        {tab === "chat" && (
          <MockChatScreen
            title="Telegram bot integration"
            onOpenMenu={() => setDrawerOpen(true)}
          />
        )}
        {tab === "tasks" && <MockTasksScreen />}
        {tab === "analytics" && <MockAnalyticsScreen />}
        {tab === "more" && <MoreScreen />}
      </div>

      {/* Bottom tab bar — 56px + safe-area inset, 44px+ targets */}
      <nav className="flex shrink-0 border-t bg-background pb-[env(safe-area-inset-bottom)]">
        {TABS.map((t) => (
          <button
            key={t.key}
            className={cn(
              "flex h-14 flex-1 flex-col items-center justify-center gap-0.5 text-[11px] font-medium",
              tab === t.key ? "text-primary" : "text-muted-foreground"
            )}
            onClick={() => setTab(t.key)}
          >
            <t.icon className="h-5 w-5" />
            {t.label}
          </button>
        ))}
      </nav>

      {/* Conversations-only drawer (sections live in the More tab) */}
      <NavDrawer
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        conversationsOnly
        activeId={2}
      />
    </div>
  )
}

/** Minimal mock Tasks list so tab switching feels real. */
function MockTasksScreen() {
  const tasks = [
    { title: "Daily RSS digest", schedule: "Every day · 08:00", enabled: true },
    { title: "Weekly budget report", schedule: "Mon · 09:30", enabled: true },
    { title: "Eval gate re-run", schedule: "Paused", enabled: false },
  ]
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex h-12 shrink-0 items-center border-b px-4 text-sm font-semibold">
        Tasks
      </header>
      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto p-3">
        {tasks.map((t) => (
          <div key={t.title} className="rounded-xl border p-3">
            <div className="flex items-center gap-2 text-sm font-medium">
              <CheckCircle2
                className={cn("h-4 w-4", t.enabled ? "text-emerald-600" : "text-muted-foreground")}
              />
              {t.title}
            </div>
            <p className="mt-1 text-xs text-muted-foreground">{t.schedule}</p>
          </div>
        ))}
      </div>
    </div>
  )
}

/** Minimal mock Analytics cards. */
function MockAnalyticsScreen() {
  const stats = [
    { label: "Runs today", value: "12" },
    { label: "Tokens", value: "48.2k" },
    { label: "Spend (30d)", value: "$4.87" },
    { label: "Tool calls", value: "311" },
  ]
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex h-12 shrink-0 items-center border-b px-4 text-sm font-semibold">
        Analytics
      </header>
      <div className="grid grid-cols-2 gap-2 p-3">
        {stats.map((s) => (
          <div key={s.label} className="rounded-xl border p-3">
            <div className="text-lg font-semibold">{s.value}</div>
            <div className="text-xs text-muted-foreground">{s.label}</div>
          </div>
        ))}
      </div>
    </div>
  )
}

/** "More" tab — the remaining sections, full-width 44px rows. */
function MoreScreen() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex h-12 shrink-0 items-center border-b px-4 text-sm font-semibold">
        More
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        <ul className="space-y-0.5">
          {mockSections.map((s) => (
            <li key={s.key}>
              <button className="flex h-12 w-full items-center gap-3 rounded-md px-3 text-sm hover:bg-accent/60">
                <s.icon className="h-4.5 w-4.5 text-muted-foreground" />
                {s.label}
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
