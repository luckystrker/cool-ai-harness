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
import type { LucideIcon } from "lucide-react"

export interface NavItem {
  to: string
  label: string
  icon: LucideIcon
}

export interface NavGroup {
  id: string
  label: string
  items: readonly NavItem[]
}

/** Section navigation shared by the desktop sidebar footer, the mobile
 *  drawer, and the mobile top app bar (title lookup). */
export const NAV_GROUPS: readonly NavGroup[] = [
  {
    id: "knowledge",
    label: "Knowledge",
    items: [
      { to: "/memory", label: "Memory", icon: Brain },
      { to: "/wiki", label: "Wiki", icon: BookOpen },
    ],
  },
  {
    id: "agents",
    label: "Agents",
    items: [
      { to: "/profiles", label: "Profiles", icon: Settings2 },
      { to: "/subagents", label: "Subagents", icon: Bot },
    ],
  },
  {
    id: "workflows",
    label: "Workflows",
    items: [
      { to: "/deep-research", label: "Deep Research", icon: SearchCheck },
      { to: "/tasks", label: "Tasks", icon: CalendarClock },
    ],
  },
  {
    id: "operations",
    label: "Operations",
    items: [
      { to: "/analytics", label: "Analytics", icon: BarChart3 },
      { to: "/budgets", label: "Budgets", icon: Wallet },
      { to: "/inspector", label: "Inspector", icon: Bug },
      { to: "/settings", label: "Settings", icon: Settings },
    ],
  },
]

export const NAV_ITEMS = NAV_GROUPS.flatMap((group) => group.items)
