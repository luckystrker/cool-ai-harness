/**
 * UI theme preference: light | dark | system. Stored in localStorage and
 * applied as the `.dark` class on <html>. Inside a Telegram WebApp,
 * telegramWebApp.ts applies Telegram's colorScheme on top (it wins whenever
 * the host app reports a themeChanged event).
 */

export type ThemePreference = "light" | "dark" | "system"

const STORAGE_KEY = "cool.theme"

export function loadThemePreference(): ThemePreference {
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    return stored === "light" || stored === "dark" || stored === "system"
      ? stored
      : "system"
  } catch {
    return "system"
  }
}

export function applyThemePreference(pref: ThemePreference): void {
  const dark =
    pref === "dark" ||
    (pref === "system" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches)
  document.documentElement.classList.toggle("dark", dark)
}

export function saveThemePreference(pref: ThemePreference): void {
  try {
    localStorage.setItem(STORAGE_KEY, pref)
  } catch {
    /* localStorage unavailable — non-fatal */
  }
  applyThemePreference(pref)
}

/** Apply the stored preference at boot and track OS changes while on "system". */
export function initTheme(): void {
  if (typeof window === "undefined") return
  applyThemePreference(loadThemePreference())
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", () => {
      if (loadThemePreference() === "system") applyThemePreference("system")
    })
}
