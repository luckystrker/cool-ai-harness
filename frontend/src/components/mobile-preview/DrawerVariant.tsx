import { useState } from "react"
import { MockChatScreen } from "./MockChatScreen"
import { NavDrawer } from "./NavDrawer"

/**
 * Variant A — slide-over drawer navigation.
 * The hamburger in the chat header opens one drawer containing everything:
 * new chat, projects, conversations, and the full section nav. Simplest
 * mapping of the desktop sidebar to mobile; nothing is permanently visible.
 */
export function DrawerVariant() {
  const [drawerOpen, setDrawerOpen] = useState(false)

  return (
    <div className="relative flex h-full min-h-0 flex-col overflow-hidden">
      <MockChatScreen
        title="Telegram bot integration"
        onOpenMenu={() => setDrawerOpen(true)}
      />
      <NavDrawer
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        activeId={2}
      />
    </div>
  )
}
