import { useState } from "react"
import { Outlet, useLocation } from "react-router-dom"
import { MobileAppBar } from "@/components/layout/MobileAppBar"
import { MobileDrawer } from "@/components/layout/MobileDrawer"
import { Sidebar } from "@/components/layout/Sidebar"
import { MobileNavContext } from "@/hooks/useMobileNav"
import { useIsMobile } from "@/hooks/useMediaQuery"

/**
 * App shell. Desktop (>= md): fixed sidebar + content, unchanged.
 * Mobile (< md): a hamburger opens the Material modal navigation drawer
 * (opaque, elevated); section pages get a top app bar with back + menu
 * actions. Chat screens keep their own header. `h-dvh` keeps the composer
 * above the virtual keyboard.
 */
export function AppLayout() {
  const isMobile = useIsMobile()
  const [drawerOpen, setDrawerOpen] = useState(false)
  const location = useLocation()

  // Chat screens already render their own header with a menu action.
  const isChatRoute = location.pathname === "/" || location.pathname.startsWith("/chat/")

  return (
    <MobileNavContext.Provider value={{ openDrawer: () => setDrawerOpen(true) }}>
      <div className="relative flex h-dvh w-full flex-col overflow-hidden bg-background text-foreground md:w-screen md:flex-row">
        {!isMobile && <Sidebar />}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {isMobile && !isChatRoute && <MobileAppBar />}
          <main className="min-h-0 flex-1 overflow-hidden">
            <Outlet />
          </main>
        </div>
        {isMobile && <MobileDrawer open={drawerOpen} onClose={() => setDrawerOpen(false)} />}
      </div>
    </MobileNavContext.Provider>
  )
}
