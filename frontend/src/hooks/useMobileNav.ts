import { createContext, useContext } from "react"

/**
 * Lets pages inside the mobile shell open the conversations drawer
 * (Variant B: the sidebar is hidden below `md`, navigation lives in the
 * bottom tab bar + slide-over drawer).
 */
export const MobileNavContext = createContext<{ openDrawer: () => void }>({
  openDrawer: () => {},
})

export function useMobileNav() {
  return useContext(MobileNavContext)
}
