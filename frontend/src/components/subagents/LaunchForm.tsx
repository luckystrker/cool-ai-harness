import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Rocket } from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { subagentsApi } from "@/api/subagents"
import type { SubagentRole } from "@/api/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"

interface LaunchFormProps {
  /** The parent conversation to associate launched subagents with. */
  parentConversationId: number
}

export function LaunchForm({ parentConversationId }: LaunchFormProps) {
  const queryClient = useQueryClient()
  const [prompt, setPrompt] = useState("")
  const [roleId, setRoleId] = useState<string>("")
  const [name, setName] = useState("")
  const [model, setModel] = useState("")

  const { data: roles = [] } = useQuery({
    queryKey: ["subagent-roles"],
    queryFn: subagentsApi.listRoles,
  })

  const launchMutation = useMutation({
    mutationFn: () =>
      subagentsApi.launch({
        prompt,
        parent_conversation_id: parentConversationId,
        role_id: roleId ? parseInt(roleId) : undefined,
        name: name || undefined,
        model: model || undefined,
      }),
    onSuccess: (run) => {
      queryClient.invalidateQueries({ queryKey: ["subagent-runs"] })
      toast.success(`Subagent launched: ${run.name || `#${run.id}`}`)
      setPrompt("")
      setName("")
    },
    onError: (error) =>
      toast.error("Subagent did not start", {
        description: getErrorDescription(error, "Review the task and role, then try again."),
      }),
  })

  return (
    <div className="flex flex-col gap-4 p-4">
      <h3 className="text-sm font-semibold">Launch a subagent</h3>

      <div className="grid gap-3">
        <div className="grid gap-1.5">
          <Label htmlFor="launch-prompt">Task</Label>
          <Textarea
            id="launch-prompt"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            placeholder="Describe the result this subagent should produce."
            rows={4}
          />
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="grid gap-1.5">
            <Label htmlFor="launch-role">Role</Label>
            <select
              id="launch-role"
              value={roleId}
              onChange={(e) => setRoleId(e.target.value)}
              className="h-9 rounded-md border border-input bg-transparent px-3 text-sm shadow-sm"
            >
              <option value="">Generic (no role)</option>
              {roles.map((r: SubagentRole) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="launch-name">Name (optional)</Label>
            <Input
              id="launch-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Display name"
            />
          </div>
        </div>

        <div className="grid gap-1.5">
          <Label htmlFor="launch-model">Model override (optional)</Label>
          <Input
            id="launch-model"
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder="Use the role or conversation model"
          />
        </div>
      </div>

      <Button
        onClick={() => launchMutation.mutate()}
        disabled={!prompt.trim() || launchMutation.isPending}
        className="w-full gap-2 sm:w-auto sm:self-start"
      >
        <Rocket className="h-4 w-4" />
        {launchMutation.isPending ? "Launching…" : "Launch subagent"}
      </Button>
    </div>
  )
}
