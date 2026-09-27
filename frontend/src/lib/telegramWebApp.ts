/**
 * Minimal typed wrapper around the Telegram WebApp SDK
 * (https://core.telegram.org/bots/webapps). The SPA is designed to be opened
 * both in a regular browser and inside a Telegram bot's WebApp view; this
 * module is a no-op outside Telegram.
 *
 * Responsibilities:
 *  - load the official SDK script exactly once
 *  - signal readiness and expand to the full viewport
 *  - sync the app theme with Telegram's color scheme (`.dark` class)
 *  - expose the native back button + haptic feedback helpers
 *
 * Keyboard handling: the mobile layout uses a flex column with `dvh` sizing,
 * so when the virtual keyboard opens, Telegram shrinks `viewportStableHeight`
 * and the composer stays visible above it — no extra JS needed here.
 */

export interface TelegramWebApp {
  ready(): void
  expand(): void
  isExpanded: boolean
  viewportStableHeight: number
  colorScheme: "light" | "dark"
  platform: string
  version: string
  setHeaderColor?(color: string): void
  setBackgroundColor?(color: string): void
  close(): void
  BackButton: { isVisible: boolean; show(): void; hide(): void; onClick(cb: () => void): void }
  HapticFeedback: {
    impactOccurred(style: "light" | "medium" | "heavy" | "rigid" | "soft"): void
    notificationOccurred(type: "error" | "success" | "warning"): void
  }
  onEvent(event: "themeChanged" | "viewportChanged" | "backButtonClicked", cb: () => void): void
}

declare global {
  interface Window {
    Telegram?: { WebApp?: TelegramWebApp }
  }
}

const SDK_URL = "https://telegram.org/js/telegram-web-app.js"

/** True when the SPA runs inside a Telegram WebApp view. */
export function isTelegram(): boolean {
  return typeof window !== "undefined" && Boolean(window.Telegram?.WebApp)
}

function tg(): TelegramWebApp | null {
  return window.Telegram?.WebApp ?? null
}

/** Apply Telegram's color scheme to the app's `.dark` theme class. */
function applyTheme(scheme: "light" | "dark") {
  document.documentElement.classList.toggle("dark", scheme === "dark")
}

let initialized = false

/**
 * Cheap heuristic for "this page is probably inside a Telegram WebApp view".
 * Avoids fetching the SDK (and the postEvent console spam) in a regular
 * browser: Telegram launches web apps with `tgWebAppData` in the URL fragment
 * and injects a bridge object (WebApp / TelegramWebviewProxy) early.
 */
function maybeTelegramEnvironment(): boolean {
  if (typeof window === "undefined") return false
  return (
    Boolean(window.Telegram?.WebApp) ||
    "TelegramWebviewProxy" in window ||
    window.location.hash.includes("tgWebAppData=") ||
    /Telegram/i.test(navigator.userAgent)
  )
}

/**
 * Load the SDK (only when the page looks like it's inside Telegram) and wire
 * theme/viewport integration. Does nothing in a regular browser.
 */
export function initTelegramWebApp(): void {
  if (initialized || typeof window === "undefined") return
  initialized = true
  if (!maybeTelegramEnvironment()) return

  const boot = () => {
    const app = tg()
    if (!app) return
    try {
      app.ready()
      app.expand()
      applyTheme(app.colorScheme)
      app.onEvent("themeChanged", () => applyTheme(app.colorScheme))
    } catch {
      /* SDK quirks must never break the app */
    }
  }

  if (window.Telegram?.WebApp) {
    boot()
    return
  }

  const script = document.createElement("script")
  script.src = SDK_URL
  script.async = true
  script.onload = boot
  document.head.appendChild(script)
}

/** Show Telegram's native back button (e.g. while a drawer/sheet is open). */
export function showTelegramBackButton(onClick: () => void): void {
  const app = tg()
  if (!app) return
  try {
    app.BackButton.show()
    app.BackButton.onClick(onClick)
  } catch {
    /* non-fatal */
  }
}

export function hideTelegramBackButton(): void {
  const app = tg()
  if (!app) return
  try {
    app.BackButton.hide()
  } catch {
    /* non-fatal */
  }
}

/** Light haptic tick for touch interactions (no-op outside Telegram). */
export function hapticTick(): void {
  const app = tg()
  if (!app) return
  try {
    app.HapticFeedback.impactOccurred("light")
  } catch {
    /* non-fatal */
  }
}
