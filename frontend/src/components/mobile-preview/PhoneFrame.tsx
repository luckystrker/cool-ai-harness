import { cn } from "@/lib/utils"

/**
 * Device bezel used on the desktop comparison view. Renders a fixed-width
 * phone-shaped frame with a status bar strip; children fill the screen area.
 * In "full-size" mode (no frame) the variant renders directly instead.
 */
export function PhoneFrame({
  width,
  label,
  children,
  className,
}: {
  /** Frame content width in px (320 / 375 / 768 presets). */
  width: number
  label?: string
  children: React.ReactNode
  className?: string
}) {
  return (
    <div className={cn("flex flex-col items-center gap-2", className)}>
      {label && (
        <span className="text-sm font-medium text-muted-foreground">{label}</span>
      )}
      <div
        className="relative overflow-hidden rounded-[2rem] border-[6px] border-zinc-800 bg-background shadow-xl"
        style={{ width, height: "min(780px, calc(100dvh - 160px))" }}
      >
        {/* Notch / status bar strip */}
        <div className="pointer-events-none absolute inset-x-0 top-0 z-40 flex h-6 items-center justify-between bg-background/90 px-5 text-[10px] font-medium text-muted-foreground">
          <span>9:41</span>
          <div className="h-3 w-16 rounded-full bg-zinc-800" />
          <span>100%</span>
        </div>
        {/* Screen area — 100dvh-style flex layout so the composer stays pinned
            above the virtual keyboard on real devices. */}
        <div className="flex h-full flex-col pt-6">{children}</div>
      </div>
    </div>
  )
}
