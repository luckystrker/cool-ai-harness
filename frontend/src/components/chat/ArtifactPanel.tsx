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
} from "lucide-react"
import { toast } from "sonner"
import { artifactsApi } from "@/api/artifacts"
import type { Artifact, ArtifactKind } from "@/api/types"
import { Button } from "@/components/ui/button"
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
}

/** Side panel listing all artifacts for a conversation with download/delete actions. */
export function ArtifactPanel({ conversationId }: ArtifactPanelProps) {
  const queryClient = useQueryClient()

  const { data: artifacts = [], isLoading } = useQuery({
    queryKey: ["artifacts", conversationId],
    queryFn: () => artifactsApi.list(conversationId),
  })

  const deleteMutation = useMutation({
    mutationFn: (artifactId: number) => artifactsApi.delete(conversationId, artifactId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["artifacts", conversationId] })
    },
    onError: (e) => toast.error("Failed to delete artifact", { description: String(e) }),
  })

  return (
    <div className="flex h-full flex-col border-l bg-muted/20">
      <div className="flex h-14 items-center border-b px-4">
        <h2 className="text-sm font-medium">Attachments</h2>
        <span className="ml-2 rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
          {artifacts.length}
        </span>
      </div>

      <ScrollArea className="flex-1">
        {isLoading ? (
          <div className="py-8 text-center text-sm text-muted-foreground">Loading…</div>
        ) : artifacts.length === 0 ? (
          <div className="px-4 py-8 text-center text-sm text-muted-foreground">
            No attachments yet. Use the 📎 button in the composer to upload files.
          </div>
        ) : (
          <ul className="space-y-1 p-2">
            {artifacts.map((a) => (
              <ArtifactRow
                key={a.id}
                artifact={a}
                conversationId={conversationId}
                onDelete={() => deleteMutation.mutate(a.id)}
              />
            ))}
          </ul>
        )}
      </ScrollArea>
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
      <Icon className="h-4 w-4 shrink-0 text-muted-foreground" />
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
      <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
        <a href={downloadHref} download={artifact.filename}>
          <Button size="icon" variant="ghost" className="h-7 w-7" title="Download">
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

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
