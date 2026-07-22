import { ShieldAlert } from "lucide-react"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import type { PendingApproval } from "@/hooks/useConversationStream"

interface ApprovalDialogProps {
  approval: PendingApproval | null
  onRespond: (approved: boolean) => void
}

/**
 * Modal shown when the agent wants to run a tool gated behind an "ask"
 * permission. The agent loop is blocked server-side until the user decides.
 */
export function ApprovalDialog({ approval, onRespond }: ApprovalDialogProps) {
  const open = approval !== null
  const argsJson =
    approval && Object.keys(approval.arguments).length > 0
      ? JSON.stringify(approval.arguments, null, 2)
      : null

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        // Closing via the X / overlay is treated as a denial so the loop
        // doesn't hang waiting on a dismissed dialog.
        if (!o && approval) onRespond(false)
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <ShieldAlert className="h-5 w-5 text-amber-500" />
            Approve tool call?
          </DialogTitle>
          <DialogDescription>
            The agent wants to run a tool that requires your approval.
          </DialogDescription>
        </DialogHeader>

        {approval && (
          <div className="space-y-2 text-sm">
            <div className="flex items-center gap-2">
              <span className="text-muted-foreground">Tool:</span>
              <span className="font-mono font-medium">{approval.name}</span>
            </div>
            {argsJson && (
              <div>
                <div className="mb-1 text-muted-foreground">Arguments</div>
                <pre className="max-h-56 overflow-auto rounded bg-muted p-2 font-mono text-[11px]">
                  {argsJson}
                </pre>
              </div>
            )}
            {approval.reason && (
              <p className="text-muted-foreground">{approval.reason}</p>
            )}
          </div>
        )}

        <DialogFooter>
          <Button variant="destructive" onClick={() => onRespond(false)}>
            Deny
          </Button>
          <Button onClick={() => onRespond(true)}>Allow</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
