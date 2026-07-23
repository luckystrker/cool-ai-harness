import { Component, type ErrorInfo, type ReactNode } from "react"
import { AlertTriangle, RefreshCw } from "lucide-react"
import { Button } from "@/components/ui/button"

interface Props {
  children: ReactNode
}

interface State {
  error: Error | null
}

/**
 * Top-level React error boundary.
 *
 * Without this, an uncaught error during render (e.g. a tool-call block
 * receiving an unexpected payload shape) blanks the whole app to a white
 * screen, because nothing catches it. This boundary renders a recoverable
 * "something went wrong" screen with a Reload button instead, and logs the
 * error to the console so it's still visible in devtools.
 *
 * It wraps the entire app (providers, router, pages) from main.tsx so a crash
 * anywhere in the tree is contained.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // eslint-disable-next-line no-console
    console.error("Uncaught render error:", error, info.componentStack)
  }

  private handleReload = () => {
    this.setState({ error: null })
    // A full reload is the most reliable recovery: it re-mounts the app from a
    // clean state and refetches all data. We don't try to patch partial UI.
    window.location.reload()
  }

  render(): ReactNode {
    if (this.state.error) {
      return (
        <div className="flex h-screen w-screen flex-col items-center justify-center gap-4 bg-background p-8 text-center">
          <div className="flex h-14 w-14 items-center justify-center rounded-2xl bg-destructive/10">
            <AlertTriangle className="h-7 w-7 text-destructive" />
          </div>
          <div className="space-y-1">
            <h1 className="text-lg font-semibold">Something went wrong</h1>
            <p className="max-w-md text-sm text-muted-foreground">
              The interface hit an unexpected error while rendering. Reloading
              usually fixes it; your conversation history is saved on the server.
            </p>
          </div>
          <pre className="max-w-lg overflow-auto rounded-md bg-muted p-3 text-left font-mono text-[11px] text-muted-foreground">
            {this.state.error.message || String(this.state.error)}
          </pre>
          <Button onClick={this.handleReload} className="gap-2">
            <RefreshCw className="h-4 w-4" />
            Reload
          </Button>
        </div>
      )
    }
    return this.props.children
  }
}
