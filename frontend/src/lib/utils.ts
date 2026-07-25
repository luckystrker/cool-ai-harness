import { type ClassValue, clsx } from "clsx"
import { twMerge } from "tailwind-merge"

/** Merge Tailwind classes, resolving conflicts. Used by all UI components. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/** Format a millisecond duration compactly: "340ms", "1.2s", "1m 03s". */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`
  const s = ms / 1000
  if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)}s`
  const m = Math.floor(s / 60)
  const rem = Math.round(s % 60)
  return `${m}m ${String(rem).padStart(2, "0")}s`
}

/**
 * Compact timestamp for a message header. Shows HH:MM for today's messages,
 * otherwise a short date. Returns "" for invalid input.
 */
export function formatMessageTime(iso: string | null | undefined): string {
  if (!iso) return ""
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ""
  const now = new Date()
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate()
  const hh = String(d.getHours()).padStart(2, "0")
  const mm = String(d.getMinutes()).padStart(2, "0")
  if (sameDay) return `${hh}:${mm}`
  // Short date: "Jul 25, 14:32"
  const month = d.toLocaleString("en", { month: "short" })
  return `${month} ${d.getDate()}, ${hh}:${mm}`
}
