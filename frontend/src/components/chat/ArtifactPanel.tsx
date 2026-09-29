import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  Download,
  File,
  FileCode,
  FileImage,
  FileText,
  Music,
  Trash2,
  FileSpreadsheet,
  X,
} from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { artifactsApi } from "@/api/artifacts"
import type { Artifact, ArtifactKind } from "@/api/types"
import { Button } from "@/components/ui/button"
import { ConfirmDialog } from "@/components/ui/confirm-dialog"
import { QueryErrorState } from "@/components/ui/query-state"
import { ScrollArea } from "@/components/ui/scroll-area"

const KIND_ICON: Record<ArtifactKind, typeof File> = {
  file: File,
  image: FileImage,
  document: FileText,
  code: FileCode,
  report: FileSpreadsheet,
  audio: Music,
  tool_result: File,
}

const KIND_LABEL: Record<ArtifactKind, string> = {
  file: "File",
  image: "Image",
  document: "Document",
  code: "Code",
  report: "Report",
  audio: "Audio",
  tool_result: "Tool result",
}

interface ArtifactPanelProps {
  conversationId: number
  /** When set, shows an X button in the header (the panel is an overlay on mobile). */
  onClose?: () => void
  /** Files staged in the composer that have not been sent yet. */
  pendingFiles?: File[]
  /** Removes a staged file by index (same ordering as `pendingFiles`). */
  onRemovePending?: (index: number) => void
}

/** Side panel listing all artifacts for a conversation with download/delete actions. */
export function ArtifactPanel({
  conversationId,
  onClose,
  pendingFiles = [],
  onRemovePending,
}: ArtifactPanelProps) {
  const queryClient = useQueryClient()
  const [deleteTarget, setDeleteTarget] = useState<Artifact | null>(null)

  const { data: artifacts = [], isLoading, isError, refetch } = useQuery({
    queryKey: ["artifacts", conversationId],
    queryFn: () => artifactsApi.list(conversationId),
  })

  const deleteMutation = useMutation({
    mutationFn: (artifactId: number) => artifactsApi.delete(conversationId, artifactId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["artifacts", conversationId] })
      setDeleteTarget(null)
    },
    onError: (error) =>
      toast.error("Attachment was not deleted", {
        description: getErrorDescription(error, "Refresh the attachment list and try again."),
      }),
  })

  return (
    <div className="flex h-full flex-col border-l bg-muted/20">
      <div className="flex h-14 items-center border-b px-4">
        <h2 className="text-sm font-medium">Attachments</h2>
        <span className="ml-2 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
          {artifacts.length + pendingFiles.length}
        </span>
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            className="ml-auto grid h-9 w-9 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            title="Close attachments"
            aria-label="Close attachments panel"
          >
            <X className="h-4 w-4" />
          </button>
        )}
      </div>

      <ScrollArea className="flex-1">
        {pendingFiles.length > 0 && (
          <div className="border-b">
            <p className="px-4 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
              Staged — sends with your next message
            </p>
            <ul className="space-y-1 p-2">
              {pendingFiles.map((file, index) => (
                <PendingFileRow
                  key={`${file.name}-${index}`}
                  file={file}
                  onRemove={
                    onRemovePending ? () => onRemovePending(index) : undefined
                  }
                />
              ))}
            </ul>
          </div>
        )}
        {isError ? (
          <QueryErrorState
            compact
            title="Attachments could not be loaded"
            description="Check that Cool is running locally, then try again."
            onRetry={() => void refetch()}
          />
        ) : isLoading ? (
          <div className="py-8 text-center text-sm text-muted-foreground">Loading attachments…</div>
        ) : artifacts.length === 0 ? (
          pendingFiles.length === 0 && (
            <div className="px-4 py-8 text-center text-sm text-muted-foreground">
              No attachments yet. Use “Attach files” in the composer to add context.
            </div>
          )
        ) : (
          <ul className="space-y-1 p-2">
            {artifacts.map((a) => (
              <ArtifactRow
                key={a.id}
                artifact={a}
                conversationId={conversationId}
                onDelete={() => setDeleteTarget(a)}
              />
            ))}
          </ul>
        )}
      </ScrollArea>

      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
        title="Delete this attachment?"
        description={
          deleteTarget
            ? `\u201c${deleteTarget.filename}\u201d will be permanently deleted.`
            : ""
        }
        confirmLabel="Delete attachment"
        pending={deleteMutation.isPending}
        onConfirm={() => deleteTarget && deleteMutation.mutate(deleteTarget.id)}
      />
    </div>
  )
}

function ArtifactRow({
  artifact,
  conversationId,
  onDelete,
}: {
  artifact: Artifact
  conversationId: number
  onDelete: () => void
}) {
  const Icon = KIND_ICON[artifact.kind] ?? File
  const downloadHref = artifactsApi.downloadUrl(conversationId, artifact.id)

  return (
    <li className="group flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-accent/60">
      {artifact.kind === "image" ? (
        <img
          src={downloadHref}
          alt=""
          className="h-10 w-10 shrink-0 rounded border object-cover"
          loading="lazy"
        />
      ) : (
        <Icon className="h-4 w-4 shrink-0 text-muted-foreground" />
      )}
      <div className="min-w-0 flex-1">
        <a
          href={downloadHref}
          className="block truncate text-sm hover:underline"
          title={artifact.filename}
          download={artifact.filename}
        >
          {artifact.filename}
        </a>
        <span className="text-[11px] text-muted-foreground">
          {KIND_LABEL[artifact.kind]} · {formatSize(artifact.size_bytes)}
          {artifact.version > 1 && ` · v${artifact.version}`}
        </span>
      </div>
      <div className="pointer-events-none flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity focus-within:pointer-events-auto focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100">
        <a href={downloadHref} download={artifact.filename}>
          <Button
            size="icon"
            variant="ghost"
            className="h-7 w-7"
            title="Download"
            aria-label="Download artifact"
          >
            <Download className="h-3.5 w-3.5" />
          </Button>
        </a>
        <Button
          size="icon"
          variant="ghost"
          className="h-7 w-7 text-muted-foreground hover:text-destructive"
          title="Delete"
          onClick={onDelete}
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
    </li>
  )
}

function PendingFileRow({
  file,
  onRemove,
}: {
  file: File
  onRemove?: () => void
}) {
  const Icon = file.type.startsWith("image/") ? FileImage : File
  return (
    <li className="group flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-accent/60">
      <Icon className="h-4 w-4 shrink-0 text-muted-foreground" />
      <div className="min-w-0 flex-1">
        <span className="block truncate text-sm" title={file.name}>
          {file.name}
        </span>
        <span className="text-[11px] text-muted-foreground">{formatSize(file.size)}</span>
      </div>
      {onRemove && (
        <Button
          size="icon"
          variant="ghost"
          className="h-7 w-7 text-muted-foreground hover:text-destructive"
          title="Remove staged file"
          aria-label={`Remove staged file ${file.name}`}
          onClick={onRemove}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      )}
    </li>
  )
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
