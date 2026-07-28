import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Users, Plus, Pencil, Trash2, Loader2 } from "lucide-react"
import { toast } from "sonner"
import { profilesApi } from "@/api/profiles"
import type { AgentProfile, ProfileCreate, ProfileUpdate } from "@/api/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Textarea } from "@/components/ui/textarea"

export function ProfilesPage() {
  const queryClient = useQueryClient()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<AgentProfile | null>(null)

  const { data: profiles = [], isLoading } = useQuery({
    queryKey: ["profiles"],
    queryFn: () => profilesApi.list(true),
  })

  const createMutation = useMutation({
    mutationFn: (body: ProfileCreate) => profilesApi.create(body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["profiles"] })
      toast.success("Profile created")
      setDialogOpen(false)
    },
    onError: (e) => toast.error("Failed to create profile", { description: String(e) }),
  })

  const updateMutation = useMutation({
    mutationFn: ({ id, body }: { id: number; body: ProfileUpdate }) =>
      profilesApi.update(id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["profiles"] })
      toast.success("Profile updated")
      setDialogOpen(false)
      setEditing(null)
    },
    onError: (e) => toast.error("Failed to update profile", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => profilesApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["profiles"] })
      toast.success("Profile deleted")
    },
    onError: (e) => toast.error("Failed to delete profile", { description: String(e) }),
  })

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-64">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  return (
    <div className="p-6 max-w-5xl mx-auto space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Users className="h-6 w-6" />
          <h1 className="text-2xl font-bold">Agent Profiles</h1>
        </div>
        <Button
          onClick={() => {
            setEditing(null)
            setDialogOpen(true)
          }}
        >
          <Plus className="h-4 w-4 mr-1" /> New Profile
        </Button>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {profiles.map((p) => (
          <Card key={p.id} className={!p.is_active ? "opacity-50" : ""}>
            <CardHeader className="pb-2">
              <div className="flex items-center gap-2">
                <span
                  className="inline-block h-4 w-4 rounded-full shrink-0"
                  style={{ backgroundColor: p.avatar_color ?? "#6B7280" }}
                />
                <CardTitle className="text-base">{p.name}</CardTitle>
                {p.is_builtin && (
                  <Badge variant="secondary" className="text-xs">
                    builtin
                  </Badge>
                )}
                {!p.is_active && (
                  <Badge variant="outline" className="text-xs">
                    inactive
                  </Badge>
                )}
              </div>
              <CardDescription className="line-clamp-2">
                {p.description ?? "No description"}
              </CardDescription>
            </CardHeader>
            <CardContent className="flex items-center gap-2 pt-2">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setEditing(p)
                  setDialogOpen(true)
                }}
              >
                <Pencil className="h-3.5 w-3.5 mr-1" /> Edit
              </Button>
              {!p.is_builtin && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="text-destructive"
                  onClick={() => deleteMutation.mutate(p.id)}
                >
                  <Trash2 className="h-3.5 w-3.5 mr-1" /> Delete
                </Button>
              )}
              {p.model && (
                <span className="ml-auto text-xs text-muted-foreground">{p.model}</span>
              )}
            </CardContent>
          </Card>
        ))}
      </div>

      <ProfileDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        profile={editing}
        onCreate={(body) => createMutation.mutate(body)}
        onUpdate={(id, body) => updateMutation.mutate({ id, body })}
      />
    </div>
  )
}

function ProfileDialog({
  open,
  onOpenChange,
  profile,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (v: boolean) => void
  profile: AgentProfile | null
  onCreate: (body: ProfileCreate) => void
  onUpdate: (id: number, body: ProfileUpdate) => void
}) {
  const [name, setName] = useState("")
  const [slug, setSlug] = useState("")
  const [description, setDescription] = useState("")
  const [systemPrompt, setSystemPrompt] = useState("")
  const [model, setModel] = useState("")
  const [avatarColor, setAvatarColor] = useState("#6366F1")

  // Sync form when dialog opens with a profile to edit.
  const [lastProfile, setLastProfile] = useState<AgentProfile | null>(null)
  if (open && profile !== lastProfile) {
    setLastProfile(profile)
    setName(profile?.name ?? "")
    setSlug(profile?.slug ?? "")
    setDescription(profile?.description ?? "")
    setSystemPrompt(profile?.system_prompt ?? "")
    setModel(profile?.model ?? "")
    setAvatarColor(profile?.avatar_color ?? "#6366F1")
  }
  if (!open && lastProfile !== null) {
    setLastProfile(null)
  }

  const handleSubmit = () => {
    if (profile) {
      onUpdate(profile.id, {
        name,
        slug,
        description: description || undefined,
        system_prompt: systemPrompt || undefined,
        model: model || undefined,
        avatar_color: avatarColor,
      })
    } else {
      onCreate({
        name,
        slug: slug || name.toLowerCase().replace(/\s+/g, "-"),
        description: description || undefined,
        system_prompt: systemPrompt || undefined,
        model: model || undefined,
        avatar_color: avatarColor,
      })
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{profile ? "Edit Profile" : "New Profile"}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 pt-2">
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1">
              <Label>Name</Label>
              <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Coder" />
            </div>
            <div className="space-y-1">
              <Label>Slug</Label>
              <Input
                value={slug}
                onChange={(e) => setSlug(e.target.value)}
                placeholder="coder"
                disabled={!!profile}
              />
            </div>
          </div>
          <div className="space-y-1">
            <Label>Description</Label>
            <Input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Focused software engineering agent"
            />
          </div>
          <div className="space-y-1">
            <Label>System Prompt</Label>
            <Textarea
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              rows={8}
              placeholder="You are..."
              className="font-mono text-sm"
            />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1">
              <Label>Model (optional)</Label>
              <Input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gpt-4o"
              />
            </div>
            <div className="space-y-1">
              <Label>Avatar Color</Label>
              <div className="flex items-center gap-2">
                <input
                  type="color"
                  value={avatarColor}
                  onChange={(e) => setAvatarColor(e.target.value)}
                  className="h-9 w-12 rounded border cursor-pointer"
                />
                <span className="text-sm text-muted-foreground">{avatarColor}</span>
              </div>
            </div>
          </div>
          <div className="flex justify-end gap-2 pt-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={handleSubmit} disabled={!name.trim()}>
              {profile ? "Save" : "Create"}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
