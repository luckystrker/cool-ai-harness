import { useEffect, useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useNavigate } from "react-router-dom"
import {
  Bot,
  Copy,
  GripVertical,
  Loader2,
  Pencil,
  Play,
  Plus,
  Share2,
  Trash2,
  Wrench,
} from "lucide-react"
import { toast } from "sonner"
import { constructorApi } from "@/api/constructor"
import { profilesApi } from "@/api/profiles"
import { skillsApi } from "@/api/skills"
import type {
  AgentProfile,
  MacroStep,
  ProfileCreate,
  ProfileUpdate,
  ToolCatalogItem,
} from "@/api/types"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"

export function ProfilesPage() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [profileDialogOpen, setProfileDialogOpen] = useState(false)
  const [editing, setEditing] = useState<AgentProfile | null>(null)
  const [macroDialogOpen, setMacroDialogOpen] = useState(false)

  const { data: profiles = [], isLoading } = useQuery({
    queryKey: ["profiles"],
    queryFn: () => profilesApi.list(true),
  })
  const { data: tools = [] } = useQuery({
    queryKey: ["agent-constructor-tools"],
    queryFn: constructorApi.tools,
  })
  const { data: skillResponse } = useQuery({
    queryKey: ["skills"],
    queryFn: () => skillsApi.list(),
  })
  const { data: macros = [] } = useQuery({
    queryKey: ["agent-constructor-macros"],
    queryFn: constructorApi.macros,
  })

  const refreshProfiles = () => queryClient.invalidateQueries({ queryKey: ["profiles"] })
  const refreshMacros = async () => {
    await queryClient.invalidateQueries({ queryKey: ["agent-constructor-macros"] })
    await queryClient.invalidateQueries({ queryKey: ["agent-constructor-tools"] })
  }
  const createMutation = useMutation({
    mutationFn: (body: ProfileCreate) => profilesApi.create(body),
    onSuccess: () => {
      refreshProfiles()
      setProfileDialogOpen(false)
      toast.success("Agent blueprint created")
    },
    onError: showError("Failed to create blueprint"),
  })
  const updateMutation = useMutation({
    mutationFn: ({ id, body }: { id: number; body: ProfileUpdate }) => profilesApi.update(id, body),
    onSuccess: () => {
      refreshProfiles()
      setProfileDialogOpen(false)
      toast.success("Agent blueprint updated")
    },
    onError: showError("Failed to update blueprint"),
  })
  const deleteMutation = useMutation({
    mutationFn: profilesApi.delete,
    onSuccess: refreshProfiles,
    onError: showError("Failed to delete blueprint"),
  })
  const cloneMutation = useMutation({
    mutationFn: profilesApi.clone,
    onSuccess: () => {
      refreshProfiles()
      toast.success("Blueprint copied")
    },
    onError: showError("Failed to copy blueprint"),
  })
  const playgroundMutation = useMutation({
    mutationFn: (id: number) => profilesApi.playground(id),
    onSuccess: ({ conversation_id }) => navigate(`/chat/${conversation_id}`),
    onError: showError("Failed to launch playground"),
  })
  const createMacroMutation = useMutation({
    mutationFn: constructorApi.createMacro,
    onSuccess: () => {
      refreshMacros()
      setMacroDialogOpen(false)
      toast.success("Macro tool registered")
    },
    onError: showError("Failed to create macro tool"),
  })
  const deleteMacroMutation = useMutation({
    mutationFn: constructorApi.deleteMacro,
    onSuccess: refreshMacros,
    onError: showError("Failed to delete macro tool"),
  })

  if (isLoading) {
    return (
      <div className="grid h-64 place-items-center">
        <Loader2 className="animate-spin" />
      </div>
    )
  }

  return (
    <div className="mx-auto max-w-6xl space-y-8 p-6">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <div className="flex items-center gap-2">
            <Bot />
            <h1 className="text-2xl font-bold">Agent Constructor</h1>
          </div>
          <p className="mt-1 text-sm text-muted-foreground">
            Compose reusable blueprints from prompts, models, tools, skills, limits, and macro tools.
          </p>
        </div>
        <Button onClick={() => { setEditing(null); setProfileDialogOpen(true) }}>
          <Plus /> New blueprint
        </Button>
      </header>

      <section className="space-y-3">
        <h2 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">Blueprints</h2>
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {profiles.map((profile) => (
            <Card key={profile.id} className={!profile.is_active ? "opacity-55" : ""}>
              <CardHeader className="pb-2">
                <div className="flex items-center gap-2">
                  <span className="h-4 w-4 rounded-full" style={{ backgroundColor: profile.avatar_color ?? "#6B7280" }} />
                  <CardTitle className="text-base">{profile.name}</CardTitle>
                  {profile.is_builtin && <Badge variant="secondary">builtin</Badge>}
                  {profile.is_shared && <Badge variant="outline"><Share2 className="mr-1 h-3 w-3" />shared</Badge>}
                </div>
                <CardDescription className="line-clamp-2">{profile.description || "No description"}</CardDescription>
              </CardHeader>
              <CardContent className="space-y-3">
                <div className="flex flex-wrap gap-1">
                  <Badge variant="outline">{profile.model || "inherited model"}</Badge>
                  <Badge variant="outline">{profile.tool_names?.length ?? tools.length} tools</Badge>
                  <Badge variant="outline">{profile.skill_names?.length ?? "all"} skills</Badge>
                </div>
                <div className="flex flex-wrap gap-1">
                  <Button size="sm" variant="ghost" onClick={() => { setEditing(profile); setProfileDialogOpen(true) }}><Pencil /> Edit</Button>
                  <Button size="sm" variant="ghost" onClick={() => cloneMutation.mutate(profile.id)}><Copy /> Copy</Button>
                  <Button size="sm" variant="ghost" disabled={!profile.is_active} onClick={() => playgroundMutation.mutate(profile.id)}><Play /> Playground</Button>
                  {!profile.is_builtin && <Button size="sm" variant="ghost" className="text-destructive" onClick={() => deleteMutation.mutate(profile.id)}><Trash2 /> Delete</Button>}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      </section>

      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="flex items-center gap-2 text-lg font-semibold"><Wrench /> Macro tools</h2>
            <p className="text-sm text-muted-foreground">Validated sequential compositions; drag steps to reorder them.</p>
          </div>
          <Button variant="outline" onClick={() => setMacroDialogOpen(true)}><Plus /> New macro</Button>
        </div>
        {macros.length === 0 ? (
          <Card><CardContent className="py-8 text-center text-sm text-muted-foreground">No macro tools yet.</CardContent></Card>
        ) : (
          <div className="grid gap-3 md:grid-cols-2">
            {macros.map((macro) => (
              <Card key={macro.id}>
                <CardHeader className="pb-2">
                  <div className="flex items-center justify-between gap-2"><CardTitle className="font-mono text-base">{macro.name}</CardTitle><Badge variant={macro.is_active ? "secondary" : "outline"}>{macro.is_active ? "active" : "disabled"}</Badge></div>
                  <CardDescription>{macro.description || `${macro.steps.length} composed steps`}</CardDescription>
                </CardHeader>
                <CardContent className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>{macro.steps.map((step) => step.tool_name).join(" → ")}</span>
                  <Button size="icon" variant="ghost" className="text-destructive" onClick={() => deleteMacroMutation.mutate(macro.id)}><Trash2 /></Button>
                </CardContent>
              </Card>
            ))}
          </div>
        )}
      </section>

      <BlueprintDialog
        open={profileDialogOpen}
        onOpenChange={setProfileDialogOpen}
        profile={editing}
        tools={tools}
        skills={skillResponse?.skills.map((skill) => skill.name) ?? []}
        onSave={(body) => editing ? updateMutation.mutate({ id: editing.id, body }) : createMutation.mutate(body as ProfileCreate)}
      />
      <MacroDialog
        open={macroDialogOpen}
        onOpenChange={setMacroDialogOpen}
        tools={tools.filter((tool) => !tool.is_macro)}
        onCreate={(body) => createMacroMutation.mutate(body)}
      />
    </div>
  )
}

function BlueprintDialog({ open, onOpenChange, profile, tools, skills, onSave }: {
  open: boolean
  onOpenChange: (open: boolean) => void
  profile: AgentProfile | null
  tools: ToolCatalogItem[]
  skills: string[]
  onSave: (body: ProfileCreate | ProfileUpdate) => void
}) {
  const [name, setName] = useState("")
  const [slug, setSlug] = useState("")
  const [description, setDescription] = useState("")
  const [systemPrompt, setSystemPrompt] = useState("")
  const [model, setModel] = useState("")
  const [color, setColor] = useState("#6366F1")
  const [selectedTools, setSelectedTools] = useState<string[]>([])
  const [selectedSkills, setSelectedSkills] = useState<string[]>([])
  const [temperature, setTemperature] = useState("0.7")
  const [maxIterations, setMaxIterations] = useState("10")
  const [maxCost, setMaxCost] = useState("")
  const [shared, setShared] = useState(false)

  useEffect(() => {
    if (!open) return
    setName(profile?.name ?? "")
    setSlug(profile?.slug ?? "")
    setDescription(profile?.description ?? "")
    setSystemPrompt(profile?.system_prompt ?? "")
    setModel(profile?.model ?? "")
    setColor(profile?.avatar_color ?? "#6366F1")
    setSelectedTools(profile?.tool_names ?? tools.map((tool) => tool.name))
    setSelectedSkills(profile?.skill_names ?? skills)
    setTemperature(String(profile?.settings?.temperature ?? 0.7))
    setMaxIterations(String(profile?.settings?.max_iterations ?? 10))
    setMaxCost(profile?.settings?.max_cost_usd == null ? "" : String(profile.settings.max_cost_usd))
    setShared(profile?.is_shared ?? false)
  }, [open, profile, skills, tools])

  const submit = () => {
    const settings: Record<string, unknown> = {
      temperature: Number(temperature),
      max_iterations: Number(maxIterations),
    }
    if (maxCost) settings.max_cost_usd = Number(maxCost)
    onSave({
      name: name.trim(),
      ...(!profile ? { slug: slug.trim() || slugify(name) } : {}),
      description,
      system_prompt: systemPrompt,
      model,
      avatar_color: color,
      tool_names: selectedTools,
      skill_names: selectedSkills,
      settings,
      is_shared: shared,
    })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] max-w-3xl overflow-y-auto">
        <DialogHeader><DialogTitle>{profile ? "Edit blueprint" : "New agent blueprint"}</DialogTitle></DialogHeader>
        <div className="space-y-5">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Name"><Input value={name} onChange={(event) => setName(event.target.value)} /></Field>
            <Field label="Slug"><Input value={slug} disabled={!!profile} onChange={(event) => setSlug(event.target.value)} placeholder="research-assistant" /></Field>
          </div>
          <Field label="Description"><Input value={description} onChange={(event) => setDescription(event.target.value)} /></Field>
          <Field label="System prompt"><Textarea rows={7} className="font-mono text-sm" value={systemPrompt} onChange={(event) => setSystemPrompt(event.target.value)} /></Field>
          <div className="grid gap-3 sm:grid-cols-4">
            <Field label="Model"><Input value={model} onChange={(event) => setModel(event.target.value)} placeholder="inherit" /></Field>
            <Field label="Temperature"><Input type="number" min="0" max="2" step="0.1" value={temperature} onChange={(event) => setTemperature(event.target.value)} /></Field>
            <Field label="Max iterations"><Input type="number" min="1" max="100" value={maxIterations} onChange={(event) => setMaxIterations(event.target.value)} /></Field>
            <Field label="Max cost USD"><Input type="number" min="0" step="0.01" value={maxCost} onChange={(event) => setMaxCost(event.target.value)} placeholder="unlimited" /></Field>
          </div>
          <Chooser title={`Tools (${selectedTools.length}/${tools.length})`} items={tools.map((tool) => ({ name: tool.name, detail: tool.capabilities.join(", ") }))} selected={selectedTools} onChange={setSelectedTools} />
          <Chooser title={`Skills (${selectedSkills.length}/${skills.length})`} items={skills.map((name) => ({ name }))} selected={selectedSkills} onChange={setSelectedSkills} />
          <div className="flex items-center justify-between rounded-md border p-3">
            <div><div className="text-sm font-medium">Share inside this instance</div><div className="text-xs text-muted-foreground">Other users can copy this blueprint without changing the original.</div></div>
            <input type="checkbox" checked={shared} onChange={(event) => setShared(event.target.checked)} className="h-4 w-4" />
          </div>
          <div className="flex items-center justify-between">
            <input type="color" value={color} onChange={(event) => setColor(event.target.value)} className="h-9 w-14 rounded border" />
            <div className="flex gap-2"><Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button><Button disabled={!name.trim()} onClick={submit}>Save blueprint</Button></div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

type DraftStep = { id: string; tool_name: string; argumentsText: string }

function MacroDialog({ open, onOpenChange, tools, onCreate }: {
  open: boolean
  onOpenChange: (open: boolean) => void
  tools: ToolCatalogItem[]
  onCreate: (body: { name: string; description: string; input_schema: Record<string, unknown>; steps: MacroStep[] }) => void
}) {
  const [name, setName] = useState("macro_")
  const [description, setDescription] = useState("")
  const [inputs, setInputs] = useState("query")
  const [steps, setSteps] = useState<DraftStep[]>([])
  const [dragged, setDragged] = useState<number | null>(null)

  useEffect(() => {
    if (!open) return
    setName("macro_")
    setDescription("")
    setInputs("query")
    setSteps(tools[0] ? [{ id: "step_1", tool_name: tools[0].name, argumentsText: "{}" }] : [])
  }, [open, tools])

  const submit = () => {
    try {
      const properties = Object.fromEntries(inputs.split(",").map((value) => value.trim()).filter(Boolean).map((key) => [key, { type: "string" }]))
      onCreate({
        name,
        description,
        input_schema: { type: "object", properties },
        steps: steps.map((step) => ({ id: step.id, tool_name: step.tool_name, arguments: JSON.parse(step.argumentsText) as Record<string, unknown> })),
      })
    } catch {
      toast.error("Every step must contain valid JSON arguments")
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] max-w-2xl overflow-y-auto">
        <DialogHeader><DialogTitle>Compose macro tool</DialogTitle></DialogHeader>
        <div className="space-y-4">
          <div className="grid gap-3 sm:grid-cols-2"><Field label="Tool name"><Input value={name} onChange={(event) => setName(event.target.value)} /></Field><Field label="Input names (comma-separated)"><Input value={inputs} onChange={(event) => setInputs(event.target.value)} /></Field></div>
          <Field label="Description"><Input value={description} onChange={(event) => setDescription(event.target.value)} /></Field>
          <div className="space-y-2">
            {steps.map((step, index) => (
              <div key={`${step.id}-${index}`} draggable onDragStart={() => setDragged(index)} onDragOver={(event) => event.preventDefault()} onDrop={() => { if (dragged == null || dragged === index) return; const next = [...steps]; const [item] = next.splice(dragged, 1); next.splice(index, 0, item); setSteps(next); setDragged(null) }} className="grid gap-2 rounded-md border p-3 sm:grid-cols-[auto_1fr_1fr_auto]">
                <GripVertical className="mt-2 cursor-grab text-muted-foreground" />
                <Input value={step.id} onChange={(event) => setSteps((current) => current.map((item, i) => i === index ? { ...item, id: event.target.value } : item))} />
                <select className="h-9 rounded-md border bg-background px-2 text-sm" value={step.tool_name} onChange={(event) => setSteps((current) => current.map((item, i) => i === index ? { ...item, tool_name: event.target.value } : item))}>{tools.map((tool) => <option key={tool.name} value={tool.name}>{tool.name}</option>)}</select>
                <Button size="icon" variant="ghost" onClick={() => setSteps((current) => current.filter((_, i) => i !== index))}><Trash2 /></Button>
                <Textarea className="font-mono text-xs sm:col-start-2 sm:col-span-2" rows={3} value={step.argumentsText} onChange={(event) => setSteps((current) => current.map((item, i) => i === index ? { ...item, argumentsText: event.target.value } : item))} placeholder={'{"query":"${input.query}"}'} />
              </div>
            ))}
          </div>
          <Button variant="outline" disabled={!tools.length} onClick={() => setSteps((current) => [...current, { id: `step_${current.length + 1}`, tool_name: tools[0].name, argumentsText: "{}" }])}><Plus /> Add step</Button>
          <p className="text-xs text-muted-foreground">References: <code>${"${input.query}"}</code> and <code>${"${steps.step_1.output}"}</code>. A step may only reference earlier outputs.</p>
          <div className="flex justify-end gap-2"><Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button><Button disabled={!name.startsWith("macro_") || !steps.length} onClick={submit}>Register macro</Button></div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function Chooser({ title, items, selected, onChange }: { title: string; items: { name: string; detail?: string }[]; selected: string[]; onChange: (items: string[]) => void }) {
  return <div className="space-y-2"><div className="flex items-center justify-between"><Label>{title}</Label><Button size="sm" variant="ghost" onClick={() => onChange(selected.length === items.length ? [] : items.map((item) => item.name))}>{selected.length === items.length ? "Clear" : "Select all"}</Button></div><div className="grid max-h-44 gap-1 overflow-y-auto rounded-md border p-2 sm:grid-cols-2">{items.map((item) => <label key={item.name} className="flex items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-muted"><input type="checkbox" checked={selected.includes(item.name)} onChange={(event) => onChange(event.target.checked ? [...selected, item.name] : selected.filter((name) => name !== item.name))} /><span className="font-mono text-xs">{item.name}</span>{item.detail && <span className="ml-auto text-[10px] text-muted-foreground">{item.detail}</span>}</label>)}</div></div>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="space-y-1"><Label>{label}</Label>{children}</div>
}

function slugify(value: string) {
  return value.toLowerCase().trim().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "")
}

function showError(title: string) {
  return (error: Error) => toast.error(title, { description: String(error) })
}
