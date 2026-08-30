import { useEffect, useRef } from "react"
import * as DialogPrimitive from "@radix-ui/react-dialog"
import { useLocation } from "react-router-dom"
import { X } from "lucide-react"
import { Sidebar } from "@/components/layout/Sidebar"

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
  const returnFocusRef = useRef<HTMLElement | null>(null)

  useEffect(() => {
    if (prevPath.current !== location.pathname) onClose()
    prevPath.current = location.pathname
  }, [location.pathname, onClose])

  return (
    <DialogPrimitive.Root open={open} onOpenChange={(nextOpen) => !nextOpen && onClose()}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="motion-opacity fixed inset-0 z-40 bg-black/50 data-[state=closed]:opacity-0 data-[state=open]:opacity-100" />
        <DialogPrimitive.Content
          className="motion-spatial fixed inset-y-0 left-0 z-50 w-[min(320px,85%)] -translate-x-full rounded-r-2xl bg-background shadow-2xl outline-none transition-transform duration-200 data-[state=open]:translate-x-0"
          onOpenAutoFocus={() => {
            returnFocusRef.current = document.activeElement as HTMLElement | null
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault()
            returnFocusRef.current?.focus()
          }}
        >
          <DialogPrimitive.Title className="sr-only">Navigation</DialogPrimitive.Title>
          <DialogPrimitive.Description className="sr-only">
            Open product areas, projects, and conversations.
          </DialogPrimitive.Description>
          <DialogPrimitive.Close
            className="absolute right-2 top-2 z-10 grid h-11 w-11 place-items-center rounded-md bg-card text-muted-foreground shadow-sm transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label="Close navigation"
          >
            <X className="h-4 w-4" />
          </DialogPrimitive.Close>
          <Sidebar inDrawer className="h-full w-full rounded-r-2xl bg-background" />
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}
