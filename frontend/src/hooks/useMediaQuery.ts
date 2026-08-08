import { useEffect, useState } from "react"

/**
 * Reactive media-query match. Used by the app shell to switch between the
 * desktop layout (fixed sidebar) and the mobile layout (drawer + tab bar)
 * below the `md` breakpoint, and by Telegram WebApp adaptation.
 */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches)

  useEffect(() => {
    const mql = window.matchMedia(query)
    const onChange = (e: MediaQueryListEvent) => setMatches(e.matches)
    setMatches(mql.matches)
    mql.addEventListener("change", onChange)
    return () => mql.removeEventListener("change", onChange)
  }, [query])

  return matches
}

/** True below the `md` breakpoint (768px) — mobile layout active. */
export function useIsMobile(): boolean {
  return useMediaQuery("(max-width: 767px)")
}
