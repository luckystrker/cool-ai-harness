import { useCallback, useEffect, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { ChevronUp, Folder, HardDrive, Loader2 } from "lucide-react"
import { workspaceApi, type DirectoryListing } from "@/api/workspace"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

interface DirectoryBrowserDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Directory shown initially (falls back to the server default). */
  initialPath?: string | null
  onSelect: (path: string) => void
}

/**
 * Server-side folder browser. Lists directories via the backend
 * (/api/workspace/directories) since the working directory lives on the
 * machine running the harness, not in the browser sandbox.
 */
export function DirectoryBrowserDialog({
  open,
  onOpenChange,
  initialPath,
  onSelect,
}: DirectoryBrowserDialogProps) {
  const [path, setPath] = useState<string | undefined>(initialPath ?? undefined)

  // Reset navigation when the dialog reopens.
  useEffect(() => {
    if (open) setPath(initialPath ?? undefined)
  }, [open, initialPath])

  const { data, isLoading } = useQuery({
    queryKey: ["directories", path ?? "__default__"],
    queryFn: () => workspaceApi.directories(path),
    enabled: open,
  })

  const listing: DirectoryListing | undefined = data

  const navigate = useCallback((target: string) => setPath(target), [])

  const handleSelect = () => {
    if (listing?.current) {
      onSelect(listing.current)
      onOpenChange(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Select working directory</DialogTitle>
          <DialogDescription>
            Choose the folder the agent will use for file operations and code
            execution.
          </DialogDescription>
        </DialogHeader>

        {/* Current path bar */}
        <div className="flex items-center gap-1.5 rounded-md border bg-muted/40 px-2.5 py-1.5">
          <HardDrive className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-mono text-xs">
            {listing?.current ?? "…"}
          </span>
        </div>

        {/* Directory list */}
        <div className="min-h-[220px] rounded-md border">
          {isLoading ? (
            <div className="flex h-[220px] items-center justify-center">
              <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
            </div>
          ) : (
            <div className="max-h-[280px] overflow-y-auto p-1">
              {/* Up navigation */}
              {listing?.parent && (
                <button
                  type="button"
                  className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-accent"
                  onClick={() => navigate(listing.parent!)}
                >
                  <ChevronUp className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="text-muted-foreground">Parent directory</span>
                </button>
              )}
              {listing?.directories.map((dir) => (
                <button
                  key={dir}
                  type="button"
                  className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-accent"
                  onClick={() =>
                    navigate(
                      `${listing.current}${listing.current.endsWith("/") || listing.current.endsWith("\\") ? "" : "/"}${dir}`
                    )
                  }
                >
                  <Folder className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                  <span className="truncate">{dir}</span>
                </button>
              ))}
              {listing && listing.directories.length === 0 && (
                <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                  No sub-directories
                </p>
              )}
            </div>
          )}
        </div>

        <DialogFooter className={cn("sm:justify-between")}>
          <Button
            variant="outline"
            size="sm"
            disabled={!listing?.default}
            onClick={() => listing?.default && navigate(listing.default)}
            title={listing?.default}
          >
            Default workspace
          </Button>
          <div className="flex gap-2">
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={handleSelect} disabled={!listing?.current}>
              Select folder
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
