import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { BookOpen, Plus, Search, Pencil, Trash2, Pin } from "lucide-react"
import { toast } from "sonner"
import { getErrorDescription } from "@/api/client"
import { wikiApi } from "@/api/wiki"
import type { WikiArticle } from "@/api/types"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
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
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { ConfirmDialog } from "@/components/ui/confirm-dialog"
import { Textarea } from "@/components/ui/textarea"
import { Markdown } from "@/components/chat/Markdown"
import { QueryErrorState, QueryLoadingState } from "@/components/ui/query-state"
import { useDebouncedValue } from "@/hooks/useDebouncedValue"
import { cn } from "@/lib/utils"

export function WikiPage() {
  const queryClient = useQueryClient()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<WikiArticle | null>(null)
  const [viewing, setViewing] = useState<WikiArticle | null>(null)
  const [deleting, setDeleting] = useState<WikiArticle | null>(null)
  const [searchQuery, setSearchQuery] = useState("")
  const debouncedSearch = useDebouncedValue(searchQuery, 300)
  const [categoryFilter, setCategoryFilter] = useState<string | null>(null)

  const { data: articles = [], isLoading, isError, refetch } = useQuery({
    queryKey: ["wiki", categoryFilter],
    queryFn: () => wikiApi.list({ category: categoryFilter ?? undefined }),
  })

  const { data: categories = [] } = useQuery({
    queryKey: ["wiki-categories"],
    queryFn: () => wikiApi.categories(),
  })

  const { data: searchResults } = useQuery({
    queryKey: ["wiki-search", debouncedSearch],
    queryFn: () => wikiApi.search(debouncedSearch),
    enabled: debouncedSearch.length >= 2,
  })

  const createMutation = useMutation({
    mutationFn: (body: { title: string; content: string; category: string; tags: string[] }) =>
      wikiApi.create(body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["wiki"] })
      toast.success("Article created")
      setDialogOpen(false)
    },
    onError: (error) =>
      toast.error("Article was not created", {
        description: getErrorDescription(error, "Review the article fields and try again."),
      }),
  })

  const updateMutation = useMutation({
    mutationFn: ({ id, body }: { id: number; body: Record<string, unknown> }) =>
      wikiApi.update(id, body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["wiki"] })
      toast.success("Article updated")
      setDialogOpen(false)
      setEditing(null)
    },
    onError: (error) =>
      toast.error("Article changes were not saved", {
        description: getErrorDescription(error, "Review the article fields and try again."),
      }),
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => wikiApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["wiki"] })
      setDeleting(null)
      toast.success("Article deleted")
    },
    onError: (error) =>
      toast.error("Article was not deleted", {
        description: getErrorDescription(error, "Refresh the article list and try again."),
      }),
  })

  const pinMutation = useMutation({
    mutationFn: ({ id, is_pinned }: { id: number; is_pinned: boolean }) =>
      wikiApi.update(id, { is_pinned }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["wiki"] }),
    onError: (error) =>
      toast.error("Pinned status was not changed", {
        description: getErrorDescription(error, "Refresh the article list and try again."),
      }),
  })

  const displayArticles = debouncedSearch.length >= 2 ? (searchResults ?? []) : articles

  return (
    <div className="mx-auto max-w-4xl space-y-6 p-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <BookOpen className="h-6 w-6 text-primary" />
          <h1 className="text-2xl font-bold">Knowledge base</h1>
        </div>
        <Button onClick={() => { setEditing(null); setDialogOpen(true) }}>
          <Plus className="mr-1 h-4 w-4" /> New article
        </Button>
      </div>

      {/* Search + Category filter */}
      <div className="flex gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" />
          <Input
            placeholder="Search articles…"
            aria-label="Search knowledge base"
            className="pl-9"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>
        <select
          aria-label="Filter articles by category"
          className="rounded-md border bg-background px-3 py-2 text-sm"
          value={categoryFilter ?? ""}
          onChange={(e) => setCategoryFilter(e.target.value || null)}
        >
          <option value="">All categories</option>
          {categories.map((c) => (
            <option key={c} value={c}>{c}</option>
          ))}
        </select>
      </div>

      {/* Articles list */}
      {isLoading ? (
        <QueryLoadingState label="Loading knowledge base…" />
      ) : isError ? (
        <QueryErrorState
          title="Knowledge base could not be loaded"
          description="Check that Cool is running locally, then try again."
          onRetry={() => void refetch()}
        />
      ) : displayArticles.length === 0 ? (
        <p className="py-12 text-center text-muted-foreground">
          {debouncedSearch.length >= 2
            ? "No articles match your search. Try different words or clear the search."
            : categoryFilter
              ? `No articles in “${categoryFilter}”. Choose another category or create one.`
              : "No articles yet. Create one to build a reusable project reference."}
        </p>
      ) : (
        <div className="space-y-3">
          {displayArticles.map((article) => (
            <Card key={article.id}>
              <CardHeader className="pb-2">
                <div className="flex items-start justify-between">
                  <div>
                    <CardTitle className="text-base">
                      <button
                        type="button"
                        className="text-left hover:underline"
                        onClick={() => setViewing(article)}
                      >
                        {article.is_pinned && <Pin className="mr-1 inline h-3.5 w-3.5 text-yellow-500" />}
                        {article.title}
                      </button>
                    </CardTitle>
                    <CardDescription className="mt-1 flex items-center gap-2">
                      <Badge variant="secondary">{article.category}</Badge>
                      {article.tags.map((t) => (
                        <Badge key={t} variant="outline" className="text-xs">{t}</Badge>
                      ))}
                      <span className="text-xs text-muted-foreground">v{article.version}</span>
                    </CardDescription>
                  </div>
                  <div className="flex gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      title={article.is_pinned ? `Unpin ${article.title}` : `Pin ${article.title}`}
                      aria-label={article.is_pinned ? `Unpin ${article.title}` : `Pin ${article.title}`}
                      onClick={() => pinMutation.mutate({ id: article.id, is_pinned: !article.is_pinned })}
                    >
                      <Pin className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      title={`Edit ${article.title}`}
                      aria-label={`Edit ${article.title}`}
                      onClick={() => { setEditing(article); setDialogOpen(true) }}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      title={`Permanently delete ${article.title}`}
                      aria-label={`Permanently delete ${article.title}`}
                      onClick={() => setDeleting(article)}
                    >
                      <Trash2 className="h-3.5 w-3.5 text-red-500" />
                    </Button>
                  </div>
                </div>
              </CardHeader>
              <CardContent>
                <p className="line-clamp-2 text-sm text-muted-foreground">
                  {article.content.slice(0, 200) || "No content"}
                </p>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {/* Create/Edit dialog */}
      <ArticleDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        article={editing}
        onCreate={(body) => createMutation.mutate(body)}
        onUpdate={(id, body) => updateMutation.mutate({ id, body })}
      />

      {/* Read-only view */}
      <ArticleViewDialog
        article={viewing}
        onClose={() => setViewing(null)}
        onEdit={(a) => {
          setViewing(null)
          setEditing(a)
          setDialogOpen(true)
        }}
      />

      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title="Delete this article?"
        description={
          deleting
            ? `\u201c${deleting.title}\u201d will be permanently deleted.`
            : ""
        }
        confirmLabel="Delete article"
        pending={deleteMutation.isPending}
        onConfirm={() => deleting && deleteMutation.mutate(deleting.id)}
      />
    </div>
  )
}

function ArticleViewDialog({
  article,
  onClose,
  onEdit,
}: {
  article: WikiArticle | null
  onClose: () => void
  onEdit: (article: WikiArticle) => void
}) {
  return (
    <Dialog open={article !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[85vh] max-w-2xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{article?.title}</DialogTitle>
          <DialogDescription className="sr-only">
            Full article content with category, tags, and version.
          </DialogDescription>
        </DialogHeader>
        {article && (
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="secondary">{article.category}</Badge>
            {article.tags.map((t) => (
              <Badge key={t} variant="outline" className="text-xs">{t}</Badge>
            ))}
            <span className="text-xs text-muted-foreground">
              v{article.version}
            </span>
          </div>
        )}
        {article && <Markdown content={article.content} />}
        <div className="flex justify-end">
          <Button variant="outline" onClick={() => article && onEdit(article)}>
            <Pencil className="mr-1 h-3.5 w-3.5" /> Edit article
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function ArticleDialog({
  open,
  onOpenChange,
  article,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (v: boolean) => void
  article: WikiArticle | null
  onCreate: (body: { title: string; content: string; category: string; tags: string[] }) => void
  onUpdate: (id: number, body: Record<string, unknown>) => void
}) {
  const [title, setTitle] = useState("")
  const [content, setContent] = useState("")
  const [category, setCategory] = useState("general")
  const [tagsStr, setTagsStr] = useState("")
  const [preview, setPreview] = useState(false)

  // Re-seed the form on every open transition — covers "new after edit"
  // (article is null both times) and "different article" alike.
  const [wasOpen, setWasOpen] = useState(false)
  if (open && !wasOpen) {
    setWasOpen(true)
    setTitle(article?.title ?? "")
    setContent(article?.content ?? "")
    setCategory(article?.category ?? "general")
    setTagsStr(article?.tags.join(", ") ?? "")
    setPreview(false)
  }
  if (!open && wasOpen) {
    setWasOpen(false)
  }

  const handleSubmit = () => {
    const tags = tagsStr.split(",").map((t) => t.trim()).filter(Boolean)
    if (article) {
      onUpdate(article.id, { title, content, category, tags })
    } else {
      onCreate({ title, content, category, tags })
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] max-w-lg overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{article ? "Edit article" : "New article"}</DialogTitle>
          <DialogDescription className="sr-only">
            {article ? "Edit the knowledge base article." : "Create a knowledge base article."}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label htmlFor="wiki-article-title">Title</Label>
            <Input
              id="wiki-article-title"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Article title"
            />
            <p className="mt-1 text-xs text-muted-foreground">A title is required.</p>
          </div>
          <div>
            <div className="flex items-center justify-between">
              <Label htmlFor="wiki-article-content">Content (Markdown)</Label>
              <div className="flex gap-1 rounded-md bg-muted p-0.5" role="tablist">
                {(["write", "preview"] as const).map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    role="tab"
                    aria-selected={mode === "preview" ? preview : !preview}
                    onClick={() => setPreview(mode === "preview")}
                    className={cn(
                      "rounded px-2 py-0.5 text-xs capitalize",
                      (mode === "preview") === preview
                        ? "bg-background text-foreground shadow-sm"
                        : "text-muted-foreground"
                    )}
                  >
                    {mode}
                  </button>
                ))}
              </div>
            </div>
            {preview ? (
              <div className="min-h-[240px] rounded-md border p-3">
                {content.trim() ? (
                  <Markdown content={content} />
                ) : (
                  <p className="text-sm text-muted-foreground">Nothing to preview yet.</p>
                )}
              </div>
            ) : (
              <Textarea
                id="wiki-article-content"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                rows={10}
                placeholder="Write the article in Markdown…"
              />
            )}
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="flex-1">
              <Label htmlFor="wiki-article-category">Category</Label>
              <Input
                id="wiki-article-category"
                value={category}
                onChange={(e) => setCategory(e.target.value)}
              />
            </div>
            <div className="flex-1">
              <Label htmlFor="wiki-article-tags">Tags (comma-separated)</Label>
              <Input
                id="wiki-article-tags"
                value={tagsStr}
                onChange={(e) => setTagsStr(e.target.value)}
                placeholder="tag1, tag2"
              />
            </div>
          </div>
          <Button onClick={handleSubmit} className="w-full" disabled={!title.trim()}>
            {article ? "Save article" : "Create article"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
