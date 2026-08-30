import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Save } from "lucide-react"
import { toast } from "sonner"
import { subagentsApi } from "@/api/subagents"
import type { SubagentRole, SubagentRoleCreate } from "@/api/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"

interface RoleEditorProps {
  /** Existing role to edit, or null for creating new. */
  role: SubagentRole | null
  onSaved?: () => void
}

export function RoleEditor({ role, onSaved }: RoleEditorProps) {
  const queryClient = useQueryClient()
  const [name, setName] = useState(role?.name ?? "")
  const [description, setDescription] = useState(role?.description ?? "")
  const [systemPrompt, setSystemPrompt] = useState(role?.system_prompt ?? "")
  const [model, setModel] = useState(role?.model ?? "")
  const [selectedTools, setSelectedTools] = useState<string[]>(role?.tool_names ?? [])
  const [maxIterations, setMaxIterations] = useState(role?.max_iterations ?? 10)
  const [maxCost, setMaxCost] = useState(role?.max_cost_usd?.toString() ?? "")

  // Available tools from the backend registry (for the multi-select).
  const { data: availableTools = [] } = useQuery({
    queryKey: ["subagent-tools"],
    queryFn: subagentsApi.listTools,
  })

  const toggleTool = (tool: string) =>
    setSelectedTools((prev) =>
      prev.includes(tool) ? prev.filter((t) => t !== tool) : [...prev, tool]
    )

  const saveMutation = useMutation({
    mutationFn: async () => {
      const body: SubagentRoleCreate = {
        name,
        description: description || undefined,
        system_prompt: systemPrompt || undefined,
        model: model || undefined,
        // Empty selection = no restriction (all tools available).
        tool_names: selectedTools.length ? selectedTools : undefined,
        max_iterations: maxIterations,
        max_cost_usd: maxCost ? parseFloat(maxCost) : undefined,
      }
      if (role) {
        return subagentsApi.updateRole(role.id, body)
      }
      return subagentsApi.createRole(body)
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["subagent-roles"] })
      toast.success(role ? "Role updated" : "Role created")
      onSaved?.()
    },
    onError: (e) => toast.error("Failed to save role", { description: String(e) }),
  })

  return (
    <div className="flex flex-col gap-4 p-4">
      <h3 className="text-sm font-semibold">
        {role ? `Edit Role: ${role.name}` : "New Role"}
      </h3>

      <div className="grid gap-3">
        <div className="grid gap-1.5">
          <Label htmlFor="role-name">Name</Label>
          <Input
            id="role-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. researcher"
          />
        </div>

        <div className="grid gap-1.5">
          <Label htmlFor="role-desc">Description</Label>
          <Input
            id="role-desc"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="What this role specializes in"
          />
        </div>

        <div className="grid gap-1.5">
          <Label htmlFor="role-prompt">System Prompt</Label>
          <Textarea
            id="role-prompt"
            value={systemPrompt}
            onChange={(e) => setSystemPrompt(e.target.value)}
            placeholder="You are a specialized agent that..."
            rows={5}
            className="font-mono text-xs"
          />
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="grid gap-1.5">
            <Label htmlFor="role-model">Model (optional)</Label>
            <Input
              id="role-model"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="default model"
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="role-iter">Max Iterations</Label>
            <Input
              id="role-iter"
              type="number"
              value={maxIterations}
              onChange={(e) => setMaxIterations(parseInt(e.target.value) || 10)}
              min={1}
              max={50}
            />
          </div>
        </div>

        <div className="grid gap-1.5">
          <div className="text-sm font-medium">Allowed Tools</div>
          <p className="text-xs text-muted-foreground">
            {selectedTools.length === 0
              ? "No restriction — the subagent can use all tools."
              : `${selectedTools.length} tool(s) selected.`}
          </p>
          <div className="max-h-40 overflow-y-auto rounded-md border p-2">
            {availableTools.map((tool) => (
              <label
                key={tool}
                className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1.5 text-sm hover:bg-muted"
              >
                <input
                  type="checkbox"
                  checked={selectedTools.includes(tool)}
                  onChange={() => toggleTool(tool)}
                  className="h-3.5 w-3.5 rounded border-input"
                />
                <span className="font-mono text-xs">{tool}</span>
              </label>
            ))}
            {availableTools.length === 0 && (
              <p className="px-1.5 py-1 text-xs text-muted-foreground">Loading tools…</p>
            )}
          </div>
        </div>

        <div className="grid gap-1.5">
          <Label htmlFor="role-cost">Max Cost USD (optional)</Label>
          <Input
            id="role-cost"
            type="number"
            value={maxCost}
            onChange={(e) => setMaxCost(e.target.value)}
            placeholder="0.50"
            step="0.01"
          />
        </div>
      </div>

      <Button
        onClick={() => saveMutation.mutate()}
        disabled={!name.trim() || saveMutation.isPending}
        className="w-full gap-2 sm:w-auto sm:self-start"
      >
        <Save className="h-4 w-4" />
        {saveMutation.isPending ? "Saving..." : role ? "Update Role" : "Create Role"}
      </Button>
    </div>
  )
}
