import * as React from "react"
import { cn } from "@/lib/utils"

/**
 * Lightweight scroll area. Uses native overflow rather than a virtual
 * scroll library — plenty for chat-sized lists. Replace with Radix
 * ScrollArea if you need cross-browser scrollbar styling later.
 */
const ScrollArea = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, children, ...props }, ref) => (
  <div
    ref={ref}
    className={cn("overflow-y-auto", className)}
    {...props}
  >
    {children}
  </div>
))
ScrollArea.displayName = "ScrollArea"

export { ScrollArea }
