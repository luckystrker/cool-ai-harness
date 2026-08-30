import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  Box,
  Loader2,
  Pencil,
  Plus,
  Search,
  Trash2,
} from "lucide-react"
import { toast } from "sonner"
import { entitiesApi } from "@/api/memory"
import type { Entity } from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Textarea } from "@/components/ui/textarea"

const ENTITY_TYPES = [
  "concept",
  "person",
  "project",
  "service",
  "tool",
  "file",
  "organization",
  "other",
]

/** Named-entity registry: search, create, edit, delete. */
export function EntitiesPanel() {
  const queryClient = useQueryClient()
  const [search, setSearch] = useState("")
  const [typeFilter, setTypeFilter] = useState<string>("")
  const [editing, setEditing] = useState<Entity | null>(null)
  const [createOpen, setCreateOpen] = useState(false)

  const { data: entities = [], isLoading } = useQuery({
    queryKey: ["entities", search, typeFilter],
    queryFn: () =>
      entitiesApi.list({
        query: search || undefined,
        entity_type: typeFilter || undefined,
        limit: 200,
      }),
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => entitiesApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["entities"] })
      queryClient.invalidateQueries({ queryKey: ["memory-stats"] })
      toast.success("Entity deleted")
    },
    onError: (e) => toast.error("Failed to delete", { description: String(e) }),
  })

  return (
    <div className="flex h-full flex-col">
      <div className="grid gap-2 border-b px-4 py-3 sm:flex sm:items-center sm:px-6">
        <div className="relative min-w-0 flex-1">
          <Search className="absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search entities by name or alias…"
            aria-label="Search entities"
            className="pl-8"
          />
        </div>
        <select
          aria-label="Filter entities by type"
          value={typeFilter}
          onChange={(e) => setTypeFilter(e.target.value)}
          className="rounded-md border bg-background px-3 py-2 text-sm"
        >
          <option value="">All types</option>
          {ENTITY_TYPES.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
        <Button onClick={() => setCreateOpen(true)} className="gap-2">
          <Plus className="h-4 w-4" />
          Add entity
        </Button>
      </div>

      <ScrollArea className="flex-1">
        <div className="space-y-2 p-6">
          {isLoading ? (
            <div className="flex items-center justify-center py-12 text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin" />
            </div>
          ) : entities.length === 0 ? (
            <div className="py-12 text-center text-muted-foreground">
              <Box className="mx-auto mb-3 h-10 w-10 opacity-30" />
              <p>No entities yet.</p>
              <p className="text-sm">
                Entities are extracted from conversations or added manually.
              </p>
            </div>
          ) : (
            entities.map((entity) => (
              <EntityCard
                key={entity.id}
                entity={entity}
                onEdit={() => setEditing(entity)}
                onDelete={() => deleteMutation.mutate(entity.id)}
              />
            ))
          )}
        </div>
      </ScrollArea>

      <EntityEditDialog entity={editing} onClose={() => setEditing(null)} />
      <EntityCreateDialog open={createOpen} onClose={() => setCreateOpen(false)} />
    </div>
  )
}

function EntityCard({
  entity,
  onEdit,
  onDelete,
}: {
  entity: Entity
  onEdit: () => void
  onDelete: () => void
}) {
  return (
    <Card className="group">
      <CardContent className="flex items-start gap-4 p-4">
        <div className="flex-1 space-y-1">
          <div className="flex items-center gap-2">
            <span className="font-medium">{entity.name}</span>
            <Badge variant="secondary" className="capitalize">
              {entity.entity_type}
            </Badge>
          </div>
          {entity.aliases && entity.aliases.length > 0 && (
            <p className="text-xs text-muted-foreground">
              Also: {entity.aliases.join(", ")}
            </p>
          )}
          {entity.description && (
            <p className="text-sm leading-relaxed">{entity.description}</p>
          )}
          {entity.attributes && Object.keys(entity.attributes).length > 0 && (
            <div className="flex flex-wrap gap-1 pt-1">
              {Object.entries(entity.attributes).map(([k, v]) => (
                <Badge key={k} variant="outline" className="text-xs">
                  {k}: {String(v)}
                </Badge>
              ))}
            </div>
          )}
        </div>
        <div className="flex gap-1 opacity-100 transition-opacity md:opacity-0 md:group-focus-within:opacity-100 md:group-hover:opacity-100">
          <Button
            size="icon"
            variant="ghost"
            className="h-8 w-8"
            onClick={onEdit}
            aria-label={`Edit ${entity.name}`}
          >
            <Pencil className="h-4 w-4" />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            className="h-8 w-8 text-destructive hover:text-destructive"
            onClick={onDelete}
            aria-label={`Delete ${entity.name}`}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

function EntityFormFields({
  idPrefix,
  name,
  setName,
  entityType,
  setEntityType,
  aliases,
  setAliases,
  description,
  setDescription,
}: {
  idPrefix: string
  name: string
  setName: (v: string) => void
  entityType: string
  setEntityType: (v: string) => void
  aliases: string
  setAliases: (v: string) => void
  description: string
  setDescription: (v: string) => void
}) {
  return (
    <div className="space-y-4 py-4">
      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-name`}>Name</Label>
        <Input
          id={`${idPrefix}-name`}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-type`}>Type</Label>
        <select
          id={`${idPrefix}-type`}
          value={entityType}
          onChange={(e) => setEntityType(e.target.value)}
          className="w-full rounded-md border bg-background px-3 py-2 text-sm"
        >
          {ENTITY_TYPES.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
      </div>
      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-aliases`}>Aliases (comma-separated)</Label>
        <Input
          id={`${idPrefix}-aliases`}
          value={aliases}
          onChange={(e) => setAliases(e.target.value)}
        />
      </div>
      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-description`}>Description</Label>
        <Textarea
          id={`${idPrefix}-description`}
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          rows={2}
        />
      </div>
    </div>
  )
}

function EntityEditDialog({
  entity,
  onClose,
}: {
  entity: Entity | null
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [name, setName] = useState("")
  const [entityType, setEntityType] = useState("concept")
  const [aliases, setAliases] = useState("")
  const [description, setDescription] = useState("")

  const [lastId, setLastId] = useState<number | null>(null)
  if (entity && entity.id !== lastId) {
    setLastId(entity.id)
    setName(entity.name)
    setEntityType(entity.entity_type)
    setAliases(entity.aliases?.join(", ") ?? "")
    setDescription(entity.description ?? "")
  }

  const updateMutation = useMutation({
    mutationFn: (body: {
      name: string
      entity_type: string
      aliases: string[]
      description: string | null
    }) => entitiesApi.update(entity!.id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["entities"] })
      toast.success("Entity updated")
      onClose()
    },
    onError: (e) => toast.error("Failed to update", { description: String(e) }),
  })

  const handleSave = () => {
    if (!entity || !name.trim()) return
    updateMutation.mutate({
      name: name.trim(),
      entity_type: entityType,
      aliases: aliases
        .split(",")
        .map((a) => a.trim())
        .filter(Boolean),
      description: description.trim() || null,
    })
  }

  return (
    <Dialog open={!!entity} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Edit Entity</DialogTitle>
        </DialogHeader>
        <EntityFormFields
          idPrefix="entity-edit"
          name={name}
          setName={setName}
          entityType={entityType}
          setEntityType={setEntityType}
          aliases={aliases}
          setAliases={setAliases}
          description={description}
          setDescription={setDescription}
        />
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            onClick={handleSave}
            disabled={!name.trim() || updateMutation.isPending}
          >
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function EntityCreateDialog({
  open,
  onClose,
}: {
  open: boolean
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [name, setName] = useState("")
  const [entityType, setEntityType] = useState("concept")
  const [aliases, setAliases] = useState("")
  const [description, setDescription] = useState("")

  const createMutation = useMutation({
    mutationFn: (body: {
      name: string
      entity_type: string
      aliases: string[]
      description: string | null
    }) => entitiesApi.create(body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["entities"] })
      queryClient.invalidateQueries({ queryKey: ["memory-stats"] })
      toast.success("Entity created")
      onClose()
      setName("")
      setAliases("")
      setDescription("")
    },
    onError: (e) => toast.error("Failed to create", { description: String(e) }),
  })

  const handleCreate = () => {
    if (!name.trim()) return
    createMutation.mutate({
      name: name.trim(),
      entity_type: entityType,
      aliases: aliases
        .split(",")
        .map((a) => a.trim())
        .filter(Boolean),
      description: description.trim() || null,
    })
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Add Entity</DialogTitle>
        </DialogHeader>
        <EntityFormFields
          idPrefix="entity-create"
          name={name}
          setName={setName}
          entityType={entityType}
          setEntityType={setEntityType}
          aliases={aliases}
          setAliases={setAliases}
          description={description}
          setDescription={setDescription}
        />
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            onClick={handleCreate}
            disabled={!name.trim() || createMutation.isPending}
          >
            Create
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
