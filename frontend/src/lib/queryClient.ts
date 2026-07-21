import { QueryClient } from "@tanstack/react-query"

/**
 * A module-level QueryClient so non-component code (e.g. streaming callbacks)
 * can invalidate queries. The same instance is provided to the app tree via
 * QueryClientProvider in main.tsx.
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
})
