import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { QueryClientProvider } from "@tanstack/react-query"
import { BrowserRouter } from "react-router-dom"
import { Toaster } from "sonner"
import "./index.css"
import App from "./App.tsx"
import { ErrorBoundary } from "@/components/ErrorBoundary"
import { queryClient } from "@/lib/queryClient"

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {/* ErrorBoundary sits outside the providers/router so a render crash
        anywhere in the tree (pages, providers, even router internals) shows a
        recoverable screen instead of a blank white page. */}
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <BrowserRouter>
          <App />
          <Toaster richColors position="top-right" />
        </BrowserRouter>
      </QueryClientProvider>
    </ErrorBoundary>
  </StrictMode>
)
