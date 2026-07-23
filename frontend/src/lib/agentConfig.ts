/** Shared agent configuration constants and global-defaults persistence.
 *
 * The permission-mode presets, tool/capability name lists, and breakpoint
 * types were originally inlined in the conversation settings dialog; they
 * now live here so the composer toolbar (mode picker) and the Settings page
 * (global agent configuration) share a single source of truth.
 */

import type {
  BreakpointConfig,
  BreakpointType,
  CapabilityPolicy,
  ToolPermission,
  ToolPermissions,
} from "@/api/types"

// Tools the agent exposes (kept in sync with backend register_builtins()).
// The "*" entry is the wildcard default applied to any tool not listed.
export const TOOL_NAMES = [
  "*",
  "read_file",
  "write_file",
  "list_files",
  "python_execute",
  "web_search",
  "web_fetch",
] as const

export const CAPABILITY_NAMES = [
  "*",
  "read",
  "write",
  "execute",
  "network",
  "git",
  "send_external",
] as const

export const BREAKPOINT_TYPES: { type: BreakpointType; label: string; hint: string }[] = [
  { type: "before_tool", label: "Before tool", hint: "Pause before any tool call" },
  { type: "before_write", label: "Before write", hint: "Pause before file writes" },
  { type: "after_tool_result", label: "After result", hint: "Pause after a tool returns" },
  { type: "before_send", label: "Before send", hint: "Pause before sending to LLM" },
]

export const PERMISSIONS: ToolPermission[] = ["allow", "ask", "deny"]

export const PERM_STYLES: Record<ToolPermission, string> = {
  allow: "bg-emerald-500/15 text-emerald-600",
  ask: "bg-amber-500/15 text-amber-600",
  deny: "bg-red-500/15 text-red-600",
}

// --- Agent mode presets ---

/**
 * Quick permission presets. Selecting one writes a permission map that
 * expresses the chosen posture; the user can then fine-tune individual tools
 * in the Settings page matrix. "allow edits" runs file/list tools freely but
 * still confirms before executing code (the riskiest built-in).
 */
export type PermissionMode = "ask" | "allow" | "allow_edits"

export const MODE_PRESETS: Record<PermissionMode, ToolPermissions> = {
  ask: { "*": "ask" },
  allow: { "*": "allow" },
  allow_edits: { "*": "allow", python_execute: "ask" },
}

export const MODE_LABELS: { mode: PermissionMode; label: string; hint: string }[] = [
  { mode: "ask", label: "Always ask", hint: "Confirm every tool call" },
  { mode: "allow_edits", label: "Allow edits", hint: "Free files; confirm code" },
  { mode: "allow", label: "Always allow", hint: "Run everything without asking" },
]

/** Derive the active preset from a permission map (for highlighting). */
export function modeFromPerms(perms: ToolPermissions): PermissionMode | null {
  for (const [mode, preset] of Object.entries(MODE_PRESETS) as [PermissionMode, ToolPermissions][]) {
    const keys = Object.keys(preset)
    if (
      keys.length === Object.keys(perms).length &&
      keys.every((k) => perms[k] === preset[k])
    ) {
      return mode
    }
  }
  return null
}

// --- Global agent defaults (persisted in localStorage) ---

export interface AgentDefaults {
  permissions: ToolPermissions
  capabilityPolicy: CapabilityPolicy
  breakpoints: BreakpointConfig[]
}

const STORAGE_KEY = "harness.agentDefaults"

/** Sensible factory defaults: confirm everything. */
export const FALLBACK_AGENT_DEFAULTS: AgentDefaults = {
  permissions: { "*": "ask" },
  capabilityPolicy: {},
  breakpoints: [],
}

export function loadAgentDefaults(): AgentDefaults {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return FALLBACK_AGENT_DEFAULTS
    const parsed = JSON.parse(raw) as Partial<AgentDefaults>
    return {
      permissions: parsed.permissions ?? FALLBACK_AGENT_DEFAULTS.permissions,
      capabilityPolicy: parsed.capabilityPolicy ?? FALLBACK_AGENT_DEFAULTS.capabilityPolicy,
      breakpoints: parsed.breakpoints ?? FALLBACK_AGENT_DEFAULTS.breakpoints,
    }
  } catch {
    return FALLBACK_AGENT_DEFAULTS
  }
}

export function saveAgentDefaults(defaults: AgentDefaults): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(defaults))
}
