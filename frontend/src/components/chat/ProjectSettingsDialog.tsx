import { useEffect, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { Save } from "lucide-react"
import { updateProject, type Project } from "@/lib/projects"
import { skillsApi } from "@/api/skills"
import type { SkillInfo } from "@/api/types"
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
import { Badge } from "@/components/ui/badge"
import { cn } from "@/lib/utils"

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
  const [disabledSkills, setDisabledSkills] = useState<string[]>([])

  // Fetch available skills for the per-project toggle.
  const { data: skillsData } = useQuery({
    queryKey: ["skills"],
    queryFn: () => skillsApi.list(),
    enabled: project != null,
  })
  const allSkills: SkillInfo[] = skillsData?.skills ?? []

  // Seed the form whenever a (different) project is opened for editing.
  useEffect(() => {
    if (project) {
      setName(project.name)
      setDescription(project.description ?? "")
      setSystemInstructions(project.systemInstructions ?? "")
      setDisabledSkills(project.disabledSkills ?? [])
    }
  }, [project])

  const toggleSkill = (skillName: string) => {
    setDisabledSkills((cur) =>
      cur.includes(skillName)
        ? cur.filter((s) => s !== skillName)
        : [...cur, skillName]
    )
  }

  const handleSave = () => {
    if (!project) return
    const projects = updateProject(project.id, {
      name: name.trim() || project.name,
      description: description.trim() || undefined,
      systemInstructions: systemInstructions.trim() || undefined,
      disabledSkills: disabledSkills.length > 0 ? disabledSkills : undefined,
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

          {/* Skills: toggle global skills on/off for this project */}
          <div className="space-y-1.5">
            <div className="text-xs font-medium text-muted-foreground">Skills</div>
            <p className="text-[11px] text-muted-foreground">
              Global skills are enabled by default. Toggle off to disable for
              this project.
            </p>
            {allSkills.length === 0 ? (
              <p className="py-2 text-xs text-muted-foreground">No skills available.</p>
            ) : (
              <div className="max-h-[160px] space-y-1 overflow-y-auto rounded-md border p-2">
                {allSkills.map((skill) => {
                  const enabled = !disabledSkills.includes(skill.name)
                  return (
                    <button
                      key={skill.name}
                      type="button"
                      onClick={() => toggleSkill(skill.name)}
                      className={cn(
                        "flex w-full items-center justify-between rounded px-2 py-1.5 text-left transition-colors",
                        enabled ? "hover:bg-muted" : "opacity-50 hover:bg-muted"
                      )}
                    >
                      <div className="min-w-0">
                        <span className="font-mono text-xs">{skill.name}</span>
                        <Badge
                          variant="outline"
                          className="ml-1.5 text-[9px]"
                        >
                          {skill.source}
                        </Badge>
                      </div>
                      <span
                        className={cn(
                          "shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium",
                          enabled
                            ? "bg-emerald-500/15 text-emerald-600"
                            : "bg-red-500/15 text-red-600"
                        )}
                      >
                        {enabled ? "enabled" : "disabled"}
                      </span>
                    </button>
                  )
                })}
              </div>
            )}
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
