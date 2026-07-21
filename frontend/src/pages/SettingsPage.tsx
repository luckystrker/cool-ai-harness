import { useEffect, useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { KeyRound, Plus, Trash2, Loader2, CheckCircle2 } from "lucide-react"
import { toast } from "sonner"
import { providersApi } from "@/api/providers"
import type { Provider, ProviderCreate } from "@/api/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import { Textarea } from "@/components/ui/textarea"
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
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"

const EMPTY_FORM: ProviderCreate = {
  name: "openai",
  label: "",
  base_url: "https://api.openai.com/v1",
  api_key: "",
  default_model: "gpt-4o-mini",
  is_subscription: false,
}

export function SettingsPage() {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [form, setForm] = useState<ProviderCreate>(EMPTY_FORM)

  const { data: providers = [], isLoading } = useQuery({
    queryKey: ["providers"],
    queryFn: providersApi.list,
  })

  const createMutation = useMutation({
    mutationFn: providersApi.create,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider added")
      setOpen(false)
      setForm(EMPTY_FORM)
    },
    onError: (e) => toast.error("Failed to add provider", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: providersApi.delete,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["providers"] })
      toast.success("Provider deleted")
    },
    onError: (e) => toast.error("Failed to delete", { description: String(e) }),
  })

  const handleSubmit = () => {
    if (!form.api_key.trim()) {
      toast.error("API key is required")
      return
    }
    createMutation.mutate(form)
  }

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl space-y-6 p-6">
        <header className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="flex h-9 w-9 items-center justify-center rounded-md bg-primary text-primary-foreground">
              <KeyRound className="h-4 w-4" />
            </div>
            <div>
              <h1 className="text-lg font-semibold">Providers</h1>
              <p className="text-sm text-muted-foreground">
                Manage API keys for LLM providers. Keys are encrypted at rest.
              </p>
            </div>
          </div>

          <Dialog open={open} onOpenChange={setOpen}>
            <DialogTrigger asChild>
              <Button className="gap-2">
                <Plus className="h-4 w-4" /> Add provider
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Add provider</DialogTitle>
              </DialogHeader>
              <ProviderForm form={form} onChange={setForm} />
              <DialogFooter>
                <Button variant="outline" onClick={() => setOpen(false)}>
                  Cancel
                </Button>
                <Button onClick={handleSubmit} disabled={createMutation.isPending}>
                  {createMutation.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
                  Save
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </header>

        {isLoading ? (
          <div className="flex justify-center py-12">
            <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
          </div>
        ) : providers.length === 0 ? (
          <Card>
            <CardContent className="py-12 text-center text-sm text-muted-foreground">
              No providers yet. Click <strong>Add provider</strong> to configure one.
            </CardContent>
          </Card>
        ) : (
          <div className="space-y-3">
            {providers.map((p) => (
              <ProviderRow
                key={p.id}
                provider={p}
                onDelete={() => deleteMutation.mutate(p.id)}
                deleting={deleteMutation.isPending}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

function ProviderRow({
  provider: p,
  onDelete,
  deleting,
}: {
  provider: Provider
  onDelete: () => void
  deleting: boolean
}) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <CardTitle className="text-base">{p.label || p.name}</CardTitle>
            <Badge variant="outline" className="font-mono">{p.name}</Badge>
            {p.is_subscription && <Badge variant="secondary">subscription</Badge>}
            {p.is_active ? (
              <Badge variant="success" className="gap-1">
                <CheckCircle2 className="h-3 w-3" /> active
              </Badge>
            ) : (
              <Badge variant="warning">disabled</Badge>
            )}
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="text-muted-foreground hover:text-destructive"
            onClick={onDelete}
            disabled={deleting}
            title="Delete"
          >
            {deleting ? <Loader2 className="h-4 w-4 animate-spin" /> : <Trash2 className="h-4 w-4" />}
          </Button>
        </div>
        <CardDescription>
          {p.base_url || "(default endpoint)"} · {p.default_model || "(default model)"}
        </CardDescription>
      </CardHeader>
      <CardContent className="pt-0">
        {p.api_key_hint ? (
          <div className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
            <KeyRound className="h-3.5 w-3.5" />
            <span>{p.api_key_hint}</span>
          </div>
        ) : (
          <span className="text-xs text-muted-foreground">No key set</span>
        )}
      </CardContent>
    </Card>
  )
}

function ProviderForm({
  form,
  onChange,
}: {
  form: ProviderCreate
  onChange: (next: ProviderCreate) => void
}) {
  // Reset form when opened fresh.
  useEffect(() => {}, [form])

  const set = (patch: Partial<ProviderCreate>) => onChange({ ...form, ...patch })

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-3">
        <div className="space-y-1.5">
          <Label htmlFor="p-name">Provider</Label>
          <Input
            id="p-name"
            placeholder="openai"
            value={form.name}
            onChange={(e) => set({ name: e.target.value })}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="p-label">Label</Label>
          <Input
            id="p-label"
            placeholder="Personal"
            value={form.label ?? ""}
            onChange={(e) => set({ label: e.target.value })}
          />
        </div>
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="p-base">Base URL</Label>
        <Input
          id="p-base"
          placeholder="https://api.openai.com/v1"
          value={form.base_url ?? ""}
          onChange={(e) => set({ base_url: e.target.value })}
        />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="p-key">API key</Label>
        <Textarea
          id="p-key"
          placeholder="sk-…"
          value={form.api_key}
          onChange={(e) => set({ api_key: e.target.value })}
          className="font-mono text-xs"
          rows={2}
        />
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="p-model">Default model</Label>
        <Input
          id="p-model"
          placeholder="gpt-4o-mini"
          value={form.default_model ?? ""}
          onChange={(e) => set({ default_model: e.target.value })}
        />
      </div>
    </div>
  )
}
