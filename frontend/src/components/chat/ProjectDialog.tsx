import { useMemo, useState } from "react"
import { FolderOpen, Globe, HardDrive, Loader2, Plus } from "lucide-react"
import { toast } from "sonner"
import { conversationsApi } from "@/api/conversations"
import {
  addProject,
  newProjectId,
  setConversationProject,
  type Project,
  type ProjectType,
} from "@/lib/projects"
import { loadAgentDefaults, loadLastModel } from "@/lib/agentConfig"
import { DirectoryBrowserDialog } from "@/components/chat/DirectoryBrowserDialog"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"

interface ProjectDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Called with the newly created conversation id so the caller can navigate. */
  onCreated: (conversationId: number, project: Project) => void
}

/** Last path segment — used to suggest a project name from the folder. */
function folderName(path: string | null | undefined): string {
  if (!path) return ""
  const normalized = path.replace(/[\\/]+$/, "")
  return normalized.split(/[\\/]/).pop() || normalized
}

/**
 * "Add project" modal. A project bundles a working folder (local) plus extra
 * system instructions that apply to every chat created inside it. Remote
 * projects are a placeholder for now. Creating a chat saves the project,
 * creates a conversation wired to its folder, and links the two.
 */
export function ProjectDialog({ open, onOpenChange, onCreated }: ProjectDialogProps) {
  const [name, setName] = useState("")
  const [type, setType] = useState<ProjectType>("local")
  const [path, setPath] = useState<string | null>(null)
  const [description, setDescription] = useState("")
  const [systemInstructions, setSystemInstructions] = useState("")
  const [browserOpen, setBrowserOpen] = useState(false)
  const [creating, setCreating] = useState(false)

  const suggestedName = useMemo(() => folderName(path), [path])

  const reset = () => {
    setName("")
    setType("local")
    setPath(null)
    setDescription("")
    setSystemInstructions("")
    setCreating(false)
  }

  const handleOpenChange = (next: boolean) => {
    if (!next) reset()
    onOpenChange(next)
  }

  const canCreate = type === "remote" || Boolean(path)

  const handleCreate = async () => {
    if (!canCreate || creating) return
    setCreating(true)
    try {
      const project: Project = {
        id: newProjectId(),
        name: name.trim() || suggestedName || (type === "local" ? "New project" : "Remote project"),
        type,
        ...(type === "local" && path ? { path } : {}),
        ...(description.trim() ? { description: description.trim() } : {}),
        ...(systemInstructions.trim() ? { systemInstructions: systemInstructions.trim() } : {}),
        createdAt: new Date().toISOString(),
      }
      addProject(project)

      // Create the chat inside the project: local projects pin the working
      // directory; the last-selected model carries over; agent defaults apply.
      const defaults = loadAgentDefaults()
      const lastModel = loadLastModel()
      const conv = await conversationsApi.create({
        ...(type === "local" && path ? { working_directory: path } : {}),
        ...(lastModel ? { model: lastModel } : {}),
        permissions: defaults.permissions,
        capability_policy: defaults.capabilityPolicy,
        breakpoints: defaults.breakpoints,
      })
      setConversationProject(conv.id, project.id)
      onOpenChange(false)
      reset()
      onCreated(conv.id, project)
    } catch (e) {
      toast.error("Failed to create project chat", { description: String(e) })
      setCreating(false)
    }
  }

  return (
    <>
      <Dialog open={open} onOpenChange={handleOpenChange}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Add project</DialogTitle>
            <DialogDescription>
              Group chats under a shared folder and instructions. Creating a
              project starts a new chat inside it.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4">
            {/* Project type: local / remote */}
            <div className="space-y-1.5">
              <div className="text-xs font-medium text-muted-foreground">Type</div>
              <div className="grid grid-cols-2 gap-2">
                <button
                  type="button"
                  onClick={() => setType("local")}
                  className={cn(
                    "flex items-center gap-2 rounded-md border px-3 py-2 text-sm transition-colors",
                    type === "local"
                      ? "border-primary bg-primary/10 text-foreground"
                      : "border-muted-foreground/30 text-muted-foreground hover:bg-accent/60"
                  )}
                >
                  <HardDrive className="h-4 w-4" />
                  Local
                </button>
                <button
                  type="button"
                  onClick={() => setType("remote")}
                  className={cn(
                    "flex items-center gap-2 rounded-md border px-3 py-2 text-sm transition-colors",
                    type === "remote"
                      ? "border-primary bg-primary/10 text-foreground"
                      : "border-muted-foreground/30 text-muted-foreground hover:bg-accent/60"
                  )}
                >
                  <Globe className="h-4 w-4" />
                  Remote
                </button>
              </div>
              {type === "remote" && (
                <p className="text-xs text-muted-foreground">
                  Remote projects aren&apos;t available yet — coming soon.
                </p>
              )}
            </div>

            {/* Folder selection (local only) */}
            {type === "local" && (
              <div className="space-y-1.5">
                <div className="text-xs font-medium text-muted-foreground">
                  Project folder
                </div>
                <button
                  type="button"
                  onClick={() => setBrowserOpen(true)}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md border px-3 py-2 text-left text-sm transition-colors",
                    path
                      ? "border-muted-foreground/40 text-foreground hover:bg-accent/60"
                      : "border-dashed border-muted-foreground/40 text-muted-foreground hover:bg-accent/60"
                  )}
                >
                  <FolderOpen className="h-4 w-4 shrink-0" />
                  <span className="truncate font-mono text-xs">
                    {path ?? "Choose a folder…"}
                  </span>
                </button>
              </div>
            )}

            {/* Project name */}
            <div className="space-y-1.5">
              <Label htmlFor="project-name" className="text-xs text-muted-foreground">
                Name
              </Label>
              <Input
                id="project-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={suggestedName || "Project name"}
                className="h-8 text-sm"
              />
            </div>

            {/* Description */}
            <div className="space-y-1.5">
              <Label htmlFor="project-description" className="text-xs text-muted-foreground">
                Description
              </Label>
              <Textarea
                id="project-description"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What is this project about?"
                className="min-h-[60px] resize-y text-sm"
              />
            </div>

            {/* Extra system instructions */}
            <div className="space-y-1.5">
              <Label htmlFor="project-instructions" className="text-xs text-muted-foreground">
                Additional system instructions
              </Label>
              <Textarea
                id="project-instructions"
                value={systemInstructions}
                onChange={(e) => setSystemInstructions(e.target.value)}
                placeholder="Extra guidance applied to every agent in this project…"
                className="min-h-[80px] resize-y text-sm"
              />
            </div>
          </div>

          <DialogFooter>
            <Button variant="ghost" onClick={() => handleOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={handleCreate} disabled={!canCreate || creating}>
              {creating ? <Loader2 className="mr-1.5 h-4 w-4 animate-spin" /> : <Plus className="mr-1.5 h-4 w-4" />}
              Create chat
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <DirectoryBrowserDialog
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        onSelect={(p) => setPath(p)}
      />
    </>
  )
}
