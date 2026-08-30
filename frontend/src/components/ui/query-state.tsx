import { AlertCircle, Loader2, RefreshCw } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

export function QueryLoadingState({
  label,
  className,
}: {
  label: string
  className?: string
}) {
  return (
    <div
      className={cn(
        "flex items-center justify-center gap-2 py-12 text-sm text-muted-foreground",
        className
      )}
      role="status"
      aria-live="polite"
    >
      <Loader2 className="h-4 w-4 animate-spin" />
      <span>{label}</span>
    </div>
  )
}

export function QueryErrorState({
  title,
  description,
  onRetry,
  className,
  compact = false,
}: {
  title: string
  description: string
  onRetry: () => void
  className?: string
  compact?: boolean
}) {
  return (
    <div
      className={cn(
        "mx-auto flex max-w-lg flex-col items-center text-center",
        compact ? "gap-2 px-3 py-5" : "gap-3 px-5 py-12",
        className
      )}
      role="alert"
    >
      <div
        className={cn(
          "grid place-items-center rounded-xl bg-destructive/10 text-destructive",
          compact ? "h-8 w-8" : "h-10 w-10"
        )}
      >
        <AlertCircle className={compact ? "h-4 w-4" : "h-5 w-5"} />
      </div>
      <div className="space-y-1">
        <p className="text-sm font-medium text-foreground">{title}</p>
        <p className="text-xs leading-5 text-muted-foreground">{description}</p>
      </div>
      <Button variant="outline" size="sm" className="gap-1.5" onClick={onRetry}>
        <RefreshCw className="h-3.5 w-3.5" />
        Try again
      </Button>
    </div>
  )
}
