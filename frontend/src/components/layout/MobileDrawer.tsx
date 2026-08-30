import { useEffect, useRef } from "react"
import { useLocation } from "react-router-dom"
import { Sidebar } from "@/components/layout/Sidebar"
import { cn } from "@/lib/utils"

/**
 * Mobile modal navigation drawer (Material Design 3): an opaque, elevated
 * surface over a scrim, hosting the full Sidebar — pinned section nav on top,
 * scrollable projects/conversations below. Closes automatically when the
 * user navigates to another conversation/page, taps the scrim, or presses
 * Escape.
 */
export function MobileDrawer({ open, onClose }: { open: boolean; onClose: () => void }) {
  const location = useLocation()
  const prevPath = useRef(location.pathname)

  useEffect(() => {
    if (prevPath.current !== location.pathname) onClose()
    prevPath.current = location.pathname
  }, [location.pathname, onClose])

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose()
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [open, onClose])

  return (
    <>
      {/* Scrim */}
      <div
        className={cn(
          "absolute inset-0 z-40 bg-black/50 transition-opacity duration-200",
          open ? "opacity-100" : "pointer-events-none opacity-0"
        )}
        onClick={onClose}
        aria-hidden
      />
      {/* Panel: opaque surface + elevation, rounded outer edge (M3 modal drawer) */}
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Navigation"
        className={cn(
          "absolute inset-y-0 left-0 z-50 w-[min(320px,85%)] rounded-r-2xl shadow-2xl transition-transform duration-200",
          open ? "translate-x-0" : "-translate-x-full"
        )}
        aria-hidden={!open}
        inert={!open}
      >
        <Sidebar inDrawer className="h-full w-full rounded-r-2xl bg-background" />
      </div>
    </>
  )
}
