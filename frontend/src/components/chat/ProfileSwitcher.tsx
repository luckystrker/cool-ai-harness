import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Check, ChevronDown } from "lucide-react"
import { toast } from "sonner"
import { profilesApi } from "@/api/profiles"
import { conversationsApi } from "@/api/conversations"
import type { Conversation } from "@/api/types"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"

interface ProfileSwitcherProps {
  conversation: Conversation | null
}

export function ProfileSwitcher({ conversation }: ProfileSwitcherProps) {
  const queryClient = useQueryClient()

  const { data: profiles = [] } = useQuery({
    queryKey: ["profiles"],
    queryFn: () => profilesApi.list(),
  })

  const activeProfile = profiles.find((p) => p.id === conversation?.profile_id) ?? null

  const switchMutation = useMutation({
    mutationFn: (profileId: number | null) => {
      if (!conversation) return Promise.resolve(null)
      // Use -1 sentinel to clear profile_id on the backend.
      return conversationsApi.update(conversation.id, {
        profile_id: profileId ?? -1,
      })
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["conversations"] })
    },
    onError: (e) => toast.error("Failed to switch profile", { description: String(e) }),
  })

  if (!conversation) return null

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="sm" className="gap-1.5 text-sm">
          {activeProfile ? (
            <>
              <span
                className="inline-block h-3 w-3 rounded-full"
                style={{ backgroundColor: activeProfile.avatar_color ?? "#6B7280" }}
              />
              {activeProfile.name}
            </>
          ) : (
            "No profile"
          )}
          <ChevronDown className="h-3.5 w-3.5 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-48">
        <DropdownMenuItem onClick={() => switchMutation.mutate(null)}>
          <span className={cn("flex-1", !activeProfile && "font-medium")}>Default</span>
          {!activeProfile && <Check className="h-3.5 w-3.5" />}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        {profiles.map((p) => (
          <DropdownMenuItem key={p.id} onClick={() => switchMutation.mutate(p.id)}>
            <span
              className="inline-block h-3 w-3 rounded-full mr-2 shrink-0"
              style={{ backgroundColor: p.avatar_color ?? "#6B7280" }}
            />
            <span className={cn("flex-1", activeProfile?.id === p.id && "font-medium")}>
              {p.name}
            </span>
            {activeProfile?.id === p.id && <Check className="h-3.5 w-3.5" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
