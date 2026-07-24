import { useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { Check, ChevronDown, Cpu, Loader2, RefreshCw, Pencil } from "lucide-react"
import { toast } from "sonner"
import { providersApi } from "@/api/providers"
import type { ModelInfo, ModelsPreviewRequest } from "@/api/types"
import { formatContextWindow, formatPrice } from "@/lib/modelFormat"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { cn } from "@/lib/utils"

export interface ModelSelectProps {
  /** Current model id (controlled). */
  value: string | null | undefined
  /** Fired when the user picks a model or enters a custom one. */
  onChange: (model: string) => void
  /**
   * "preview" — probe an unsaved provider from raw form fields (create form).
   * "saved"  — list models from an already-stored provider by id (edit form).
   */
  mode: "preview" | "saved"
  /** For mode="preview": form fields + plaintext key. */
  previewRequest?: ModelsPreviewRequest
  /** For mode="saved": the stored provider id. */
  providerId?: number
  /** Disable the whole control (e.g. while a parent dialog is submitting). */
  disabled?: boolean
  id?: string
}

/**
 * Model picker that lists models served by a provider, fetched live from the
 * provider's /models endpoint. Each option shows per-1k-token price and the
 * context window where the provider returns one. Falls back to a "Custom…"
 * free-text entry when the list can't be loaded or the model isn't listed.
 *
 * In "preview" mode the list is fetched on demand (the user clicks Load) to
 * avoid hammering the provider while the API key is still being typed; in
 * "saved" mode it is fetched automatically once the provider id is known.
 */
export function ModelSelect({
  value,
  onChange,
  mode,
  previewRequest,
  providerId,
  disabled,
  id,
}: ModelSelectProps) {
  const [customOpen, setCustomOpen] = useState(false)
  const [customValue, setCustomValue] = useState("")

  const enabled =
    mode === "saved"
      ? providerId != null
      : Boolean(
          previewRequest &&
            previewRequest.api_key.trim() &&
            (previewRequest.base_url?.trim() || previewRequest.name.trim())
        )

  const { data, isFetching, refetch, error } = useQuery<ModelInfo[]>({
    queryKey:
      mode === "saved"
        ? ["provider-models", providerId]
        : ["provider-models-preview", previewRequest],
    queryFn: () =>
      mode === "saved"
        ? providersApi.listModels(providerId!)
        : providersApi.previewModels(previewRequest!),
    enabled: false, // fetch on demand (manual Load) in both modes
    retry: false,
    staleTime: 60_000,
  })

  const handleLoad = () => {
    if (!enabled) {
      toast.error("Enter API key and base URL first")
      return
    }
    refetch()
  }

  const submitCustom = () => {
    const v = customValue.trim()
    if (!v) return
    onChange(v)
    setCustomOpen(false)
    setCustomValue("")
  }

  const models = data ?? []

  return (
    <div className="space-y-1.5">
      <Label htmlFor={id}>Default model</Label>
      <div className="flex items-center gap-2">
        <DropdownMenu onOpenChange={(open) => { if (!open) setCustomOpen(false) }}>
          <DropdownMenuTrigger asChild>
            <Button
              variant="outline"
              id={id}
              disabled={disabled}
              className="h-9 w-full justify-between font-normal"
              title={value ?? "Select a model"}
            >
              <span className="flex min-w-0 items-center gap-1.5">
                <Cpu className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                <span className={cn("truncate", !value && "text-muted-foreground")}>
                  {value || "Select a model"}
                </span>
              </span>
              <ChevronDown className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-80">
            <DropdownMenuLabel className="flex items-center justify-between text-xs text-muted-foreground">
              <span>Provider models</span>
              <Button
                variant="ghost"
                size="sm"
                className="h-6 gap-1 px-1.5 text-xs"
                disabled={!enabled || isFetching}
                onClick={(e) => { e.preventDefault(); handleLoad() }}
              >
                {isFetching ? (
                  <Loader2 className="h-3 w-3 animate-spin" />
                ) : (
                  <RefreshCw className="h-3 w-3" />
                )}
                {models.length ? "Reload" : "Load"}
              </Button>
            </DropdownMenuLabel>

            {error && (
              <div className="px-2 pb-1 text-[11px] text-destructive">
                Could not load models. Use “Custom…” below.
              </div>
            )}

            {!isFetching && models.length === 0 && !error && (
              <div className="px-2 py-1 text-[11px] text-muted-foreground">
                {mode === "preview"
                  ? "Click Load to fetch the list from the provider."
                  : "Click Reload to fetch the list from the provider."}
              </div>
            )}

            {models.map((m) => (
              <DropdownMenuItem
                key={m.id}
                className="flex items-start gap-2 py-1.5"
                onSelect={(e) => {
                  e.preventDefault()
                  onChange(m.id)
                }}
              >
                {m.id === value ? (
                  <Check className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                ) : (
                  <span className="mt-0.5 inline-block h-3.5 w-3.5 shrink-0" />
                )}
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-mono text-xs">{m.id}</span>
                  <span className="mt-0.5 block text-[10px] text-muted-foreground">
                    in/out {formatPrice(m.prompt_price, m.completion_price)} · ctx {formatContextWindow(m.context_window)}
                  </span>
                </span>
              </DropdownMenuItem>
            ))}

            <DropdownMenuSeparator />

            {customOpen ? (
              <form
                className="flex items-center gap-1 p-1"
                onSubmit={(e) => {
                  e.preventDefault()
                  submitCustom()
                }}
              >
                <Input
                  autoFocus
                  placeholder="model name"
                  value={customValue}
                  onChange={(e) => setCustomValue(e.target.value)}
                  className="h-8 font-mono text-xs"
                />
                <Button type="submit" size="sm" className="h-8 px-2">
                  Set
                </Button>
              </form>
            ) : (
              <DropdownMenuItem
                className="text-xs"
                onSelect={(e) => {
                  e.preventDefault()
                  setCustomOpen(true)
                }}
              >
                <Pencil className="h-3.5 w-3.5" /> Custom model…
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  )
}
