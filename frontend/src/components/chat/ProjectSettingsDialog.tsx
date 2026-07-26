import { useEffect, useState } from "react"
import { Save } from "lucide-react"
import { updateProject, type Project } from "@/lib/projects"
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

interface ProjectSettingsDialogProps {
  /** Project being edited, or null when the dialog is closed. */
  project: Project | null
  onOpenChange: (open: boolean) => void
  /** Called with the refreshed project list after saving. */
  onSaved: (projects: Project[]) => void
}

/**
 * Per-project settings: rename the project, edit its description, and adjust
 * the extra system instructions applied to every chat inside it. The working
 * folder and type are fixed at creation time.
 */
export function ProjectSettingsDialog({
  project,
  onOpenChange,
  onSaved,
}: ProjectSettingsDialogProps) {
  const [name, setName] = useState("")
  const [description, setDescription] = useState("")
  const [systemInstructions, setSystemInstructions] = useState("")

  // Seed the form whenever a (different) project is opened for editing.
  useEffect(() => {
    if (project) {
      setName(project.name)
      setDescription(project.description ?? "")
      setSystemInstructions(project.systemInstructions ?? "")
    }
  }, [project])

  const handleSave = () => {
    if (!project) return
    const projects = updateProject(project.id, {
      name: name.trim() || project.name,
      description: description.trim() || undefined,
      systemInstructions: systemInstructions.trim() || undefined,
    })
    onSaved(projects)
    onOpenChange(false)
  }

  return (
    <Dialog open={project != null} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Project settings</DialogTitle>
          <DialogDescription>
            {project?.type === "local"
              ? `Local project · ${project.path ?? "no folder"}`
              : "Remote project"}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="project-settings-name" className="text-xs text-muted-foreground">
              Name
            </Label>
            <Input
              id="project-settings-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Project name"
              className="h-8 text-sm"
            />
          </div>

          <div className="space-y-1.5">
            <Label htmlFor="project-settings-description" className="text-xs text-muted-foreground">
              Description
            </Label>
            <Textarea
              id="project-settings-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="What is this project about?"
              className="min-h-[60px] resize-y text-sm"
            />
          </div>

          <div className="space-y-1.5">
            <Label
              htmlFor="project-settings-instructions"
              className="text-xs text-muted-foreground"
            >
              Additional system instructions
            </Label>
            <Textarea
              id="project-settings-instructions"
              value={systemInstructions}
              onChange={(e) => setSystemInstructions(e.target.value)}
              placeholder="Extra guidance applied to every agent in this project…"
              className="min-h-[80px] resize-y text-sm"
            />
          </div>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={handleSave}>
            <Save className="mr-1.5 h-4 w-4" />
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
