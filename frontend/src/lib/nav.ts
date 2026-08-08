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

/** Section navigation shared by the desktop sidebar footer, the mobile
 *  drawer, and the mobile top app bar (title lookup). */
export const NAV_ITEMS = [
  { to: "/memory", label: "Memory", icon: Brain },
  { to: "/wiki", label: "Wiki", icon: BookOpen },
  { to: "/profiles", label: "Profiles", icon: Settings2 },
  { to: "/analytics", label: "Analytics", icon: BarChart3 },
  { to: "/subagents", label: "Subagents", icon: Bot },
  { to: "/deep-research", label: "Deep Research", icon: SearchCheck },
  { to: "/tasks", label: "Tasks", icon: CalendarClock },
  { to: "/budgets", label: "Budgets", icon: Wallet },
  { to: "/inspector", label: "Inspector", icon: Bug },
  { to: "/settings", label: "Settings", icon: Settings },
]
