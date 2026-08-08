import { useState } from "react"
import { Maximize2, Minimize2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { PhoneFrame } from "@/components/mobile-preview/PhoneFrame"
import { DrawerVariant } from "@/components/mobile-preview/DrawerVariant"
import { TabBarVariant } from "@/components/mobile-preview/TabBarVariant"

const WIDTH_PRESETS = [320, 375, 768] as const

/**
 * Dev-only comparison page (/mobile-preview): renders both mobile navigation
 * variants side by side inside phone frames with a width preset switcher, or
 * one variant full-screen for testing on a real device / Telegram WebView.
 * Entirely mock-driven — no backend required.
 */
export function MobilePreviewPage() {
  const [width, setWidth] = useState<number>(375)
  const [fullscreen, setFullscreen] = useState<"A" | "B" | null>(null)

  // --- Full-screen mode: one variant fills the viewport (real-device test) ---
  if (fullscreen) {
    return (
      <div className="h-[100dvh] w-full bg-zinc-950">
        <div
          className="relative mx-auto flex h-full flex-col overflow-hidden bg-background"
          style={{ maxWidth: width }}
        >
          {fullscreen === "A" ? <DrawerVariant /> : <TabBarVariant />}
          <Button
            variant="secondary"
            size="sm"
            className="absolute right-3 top-8 z-[60] gap-1.5 shadow-lg"
            onClick={() => setFullscreen(null)}
          >
            <Minimize2 className="h-3.5 w-3.5" />
            Exit
          </Button>
        </div>
      </div>
    )
  }

  // --- Comparison mode: both frames side by side ---
  return (
    <div className="flex h-[100dvh] flex-col bg-muted/30">
      <header className="flex shrink-0 flex-wrap items-center gap-3 border-b bg-background px-4 py-3">
        <div>
          <h1 className="text-sm font-semibold">Mobile UI variants — Telegram WebApp</h1>
          <p className="text-xs text-muted-foreground">
            Interactive mocks (no backend). Pick a width preset, try both variants, then open
            full-size to test on a real device.
          </p>
        </div>
        <div className="ml-auto flex items-center gap-1 rounded-lg border bg-muted/40 p-1">
          {WIDTH_PRESETS.map((w) => (
            <button
              key={w}
              className={cn(
                "rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
                width === w
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground"
              )}
              onClick={() => setWidth(w)}
            >
              {w}px
            </button>
          ))}
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-wrap items-start justify-center gap-10 overflow-y-auto p-6">
        <div className="flex flex-col items-center gap-2">
          <PhoneFrame width={width} label={`Variant A — slide-over drawer @ ${width}px`}>
            <DrawerVariant />
          </PhoneFrame>
          <Button variant="outline" size="sm" className="gap-1.5" onClick={() => setFullscreen("A")}>
            <Maximize2 className="h-3.5 w-3.5" />
            Open full-size
          </Button>
        </div>

        <div className="flex flex-col items-center gap-2">
          <PhoneFrame width={width} label={`Variant B — bottom tab bar @ ${width}px`}>
            <TabBarVariant />
          </PhoneFrame>
          <Button variant="outline" size="sm" className="gap-1.5" onClick={() => setFullscreen("B")}>
            <Maximize2 className="h-3.5 w-3.5" />
            Open full-size
          </Button>
        </div>
      </div>
    </div>
  )
}
