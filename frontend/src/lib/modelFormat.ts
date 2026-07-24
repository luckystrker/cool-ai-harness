/** Formatting helpers for model metadata shared across the settings UI and chat. */

import type { ModelInfo } from "@/api/types"

/** 128000 -> "128K", 2000000 -> "2M", 8192 -> "8K". null/undefined -> "—". */
export function formatContextWindow(ctx: number | null | undefined): string {
  if (!ctx || ctx <= 0) return "—"
  if (ctx >= 1_000_000) {
    const m = ctx / 1_000_000
    return `${Number.isInteger(m) ? m : m.toFixed(1)}M`
  }
  if (ctx >= 1000) {
    const k = ctx / 1000
    return `${Number.isInteger(k) ? k : k.toFixed(0)}K`
  }
  return String(ctx)
}

/**
 * Compact per-1k-token price label, e.g. "$0.00015 / $0.0006".
 * Returns "—" when both prices are unknown, "?" for partial.
 */
export function formatPrice(
  prompt: number | null | undefined,
  completion: number | null | undefined
): string {
  const fmt = (v: number | null | undefined) =>
    v == null ? null : `$${v}`
  const p = fmt(prompt)
  const c = fmt(completion)
  if (p == null && c == null) return "—"
  if (p == null || c == null) return p ?? c ?? "—"
  return `${p} / ${c}`
}

/** True when a model entry carries any useful metadata (price or context). */
export function hasModelMeta(m: Pick<ModelInfo, "context_window" | "prompt_price" | "completion_price">): boolean {
  return (
    (m.context_window != null && m.context_window > 0) ||
    m.prompt_price != null ||
    m.completion_price != null
  )
}
