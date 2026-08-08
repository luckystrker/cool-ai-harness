import type { LucideIcon } from "lucide-react"
import {
  BarChart3,
  BookOpen,
  Bot,
  Brain,
  Bug,
  CalendarClock,
  SearchCheck,
  Settings,
  Settings2,
  Wallet,
} from "lucide-react"

/**
 * Static mock data for the mobile preview variants. Deliberately has no API
 * imports — the preview page must work without a running backend.
 */

export interface MockConversation {
  id: number
  title: string
  projectId: string | null
}

export interface MockProject {
  id: string
  name: string
  kind: "local" | "web"
}

export type MockMessage =
  | { type: "user"; text: string }
  | { type: "assistant"; text: string }
  | { type: "tool"; name: string; status: "done" | "running" }
  | { type: "approval"; title: string; detail: string }

export const mockProjects: MockProject[] = [
  { id: "p1", name: "cool-ai-harness", kind: "local" },
  { id: "p2", name: "landing-site", kind: "web" },
]

export const mockConversations: MockConversation[] = [
  { id: 1, title: "Fix CI flaky tests", projectId: "p1" },
  { id: 2, title: "Telegram bot integration", projectId: "p1" },
  { id: 3, title: "Refactor memory retrieval", projectId: null },
  { id: 4, title: "Landing page copy", projectId: "p2" },
  { id: 5, title: "Budget alerts design", projectId: null },
]

export const mockMessages: MockMessage[] = [
  { type: "user", text: "Can you check why the evals gate fails on CI?" },
  {
    type: "assistant",
    text: "I'll inspect the last run and compare it against the baseline traces.",
  },
  { type: "tool", name: "read_file · evals/gate.py", status: "done" },
  { type: "tool", name: "run_tests · pytest evals", status: "running" },
  {
    type: "assistant",
    text: "The gate fails because the baseline was recorded with an older scenario set. I need to write a new baseline file — approve the change below.",
  },
  {
    type: "approval",
    title: "Write file",
    detail: "evals_data/baselines/gate.json (new baseline, 4 scenarios)",
  },
]

/** Section nav shared by both variants (mirrors the desktop Sidebar footer). */
export const mockSections: { key: string; label: string; icon: LucideIcon }[] = [
  { key: "memory", label: "Memory", icon: Brain },
  { key: "wiki", label: "Wiki", icon: BookOpen },
  { key: "profiles", label: "Profiles", icon: Settings2 },
  { key: "analytics", label: "Analytics", icon: BarChart3 },
  { key: "subagents", label: "Subagents", icon: Bot },
  { key: "research", label: "Deep Research", icon: SearchCheck },
  { key: "tasks", label: "Tasks", icon: CalendarClock },
  { key: "budgets", label: "Budgets", icon: Wallet },
  { key: "inspector", label: "Inspector", icon: Bug },
  { key: "settings", label: "Settings", icon: Settings },
]
