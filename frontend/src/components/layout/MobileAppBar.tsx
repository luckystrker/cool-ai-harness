import { useLocation, useNavigate } from "react-router-dom"
import { ArrowLeft, Menu } from "lucide-react"
import { NAV_ITEMS } from "@/lib/nav"
import { Button } from "@/components/ui/button"
import { useMobileNav } from "@/hooks/useMobileNav"

/**
 * Material Design 3 top app bar for mobile section pages (Memory, Wiki, …).
 * Provides the missing navigation controls: a leading back action to the
 * chat home screen and a trailing menu action opening the nav drawer.
 * Chat screens render their own header, so the shell only mounts this bar
 * for non-chat routes.
 */
export function MobileAppBar() {
  const navigate = useNavigate()
  const location = useLocation()
  const { openDrawer } = useMobileNav()

  const title = NAV_ITEMS.find((n) => location.pathname.startsWith(n.to))?.label ?? ""

  return (
    <header className="flex h-14 shrink-0 items-center gap-1 border-b bg-background px-1">
      <Button
        variant="ghost"
        size="icon"
        className="h-11 w-11 shrink-0"
        onClick={() => navigate("/")}
        title="Back to chat"
        aria-label="Back to chat"
      >
        <ArrowLeft className="h-5 w-5" />
      </Button>
      <span className="min-w-0 flex-1 truncate text-base font-semibold">{title}</span>
      <Button
        variant="ghost"
        size="icon"
        className="h-11 w-11 shrink-0"
        onClick={openDrawer}
        title="Open menu"
        aria-label="Open menu"
      >
        <Menu className="h-5 w-5" />
      </Button>
    </header>
  )
}
