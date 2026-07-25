import { useMemo, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { Check, Loader2, RefreshCw, Search } from "lucide-react"
import { providersApi } from "@/api/providers"
import type { ModelInfo, ModelsPreviewRequest } from "@/api/types"
import { formatContextWindow, formatPrice } from "@/lib/modelFormat"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { cn } from "@/lib/utils"

export interface ChatModelsPickerProps {
  /** Selected model ids (controlled). */
  value: string[]
  onChange: (models: string[]) => void
  /**
   * "preview" — probe an unsaved provider from raw form fields (create form).
   * "saved"  — list models from an already-stored provider by id (edit form).
   */
  mode: "preview" | "saved"
  /** For mode="preview": form fields + plaintext key. */
  previewRequest?: ModelsPreviewRequest
  /** For mode="saved": the stored provider id. */
  providerId?: number
  disabled?: boolean
  id?: string
}

/**
 * Multi-select of models exposed in the chat model picker, chosen from the
 * provider's live /models list. Each row shows the model id, per-1k-token
 * price and context window where the provider returns one. The list scrolls
 * and supports filtering by name. The first selected id is used as the
 * effective default model for new conversations.
 */
export function ChatModelsPicker({
  value,
  onChange,
  mode,
  previewRequest,
  providerId,
  disabled,
  id,
}: ChatModelsPickerProps) {
  const [query, setQuery] = useState("")

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

  const models = useMemo(() => data ?? [], [data])

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return models
    return models.filter((m) => m.id.toLowerCase().includes(q))
  }, [models, query])

  const selected = new Set(value)
  const toggle = (modelId: string) => {
    if (selected.has(modelId)) {
      onChange(value.filter((m) => m !== modelId))
    } else {
      onChange([...value, modelId])
    }
  }

  const handleLoad = () => {
    if (!enabled) return
    refetch()
  }

  const hasQuery = query.trim().length > 0

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between">
        <Label htmlFor={id}>Models available in chat</Label>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-6 gap-1 px-1.5 text-xs"
          disabled={!enabled || isFetching || disabled}
          onClick={handleLoad}
        >
          {isFetching ? (
            <Loader2 className="h-3 w-3 animate-spin" />
          ) : (
            <RefreshCw className="h-3 w-3" />
          )}
          {models.length ? "Reload" : "Load models"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Pick which models appear in the chat model picker. The first selected
        model is used as the default for new conversations.
      </p>

      {/* Search box — only useful once there's a list to filter. */}
      <div className="relative">
        <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          id={id}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search models…"
          disabled={disabled || models.length === 0}
          className="h-8 pl-7 text-xs"
        />
      </div>

      <div className="max-h-60 overflow-y-auto rounded-md border">
        {error && (
          <div className="px-3 py-2 text-[11px] text-destructive">
            Could not load models. Check the API key / base URL and retry.
          </div>
        )}

        {!isFetching && models.length === 0 && !error && (
          <div className="px-3 py-3 text-center text-[11px] text-muted-foreground">
            {mode === "preview"
              ? "Click Load models to fetch the list from the provider."
              : "Click Load models to fetch the list from the provider."}
          </div>
        )}

        {filtered.map((m) => {
          const isChecked = selected.has(m.id)
          return (
            <label
              key={m.id}
              className={cn(
                "flex cursor-pointer items-start gap-2 border-b border-border/50 px-2.5 py-2 last:border-b-0",
                isChecked ? "bg-accent/40" : "hover:bg-muted/50"
              )}
            >
              <input
                type="checkbox"
                checked={isChecked}
                onChange={() => toggle(m.id)}
                disabled={disabled}
                className="mt-0.5 h-3.5 w-3.5 rounded border-input"
              />
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-1.5">
                  <span className="truncate font-mono text-xs">{m.id}</span>
                  {isChecked && <Check className="h-3 w-3 shrink-0 text-primary" />}
                </span>
                <span className="mt-0.5 block text-[10px] text-muted-foreground">
                  in/out {formatPrice(m.prompt_price, m.completion_price)} · ctx{" "}
                  {formatContextWindow(m.context_window)}
                </span>
              </span>
            </label>
          )
        })}

        {hasQuery && filtered.length === 0 && models.length > 0 && (
          <div className="px-3 py-2 text-[11px] text-muted-foreground">
            No models match “{query}”.
          </div>
        )}
      </div>

      {value.length > 0 && (
        <p className="text-[11px] text-muted-foreground">
          {value.length} selected · default: <span className="font-mono">{value[0]}</span>
        </p>
      )}
    </div>
  )
}
