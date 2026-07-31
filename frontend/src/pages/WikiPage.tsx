import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { BookOpen, Plus, Search, Pencil, Trash2, Pin, Loader2 } from "lucide-react"
import { toast } from "sonner"
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
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Textarea } from "@/components/ui/textarea"

export function WikiPage() {
  const queryClient = useQueryClient()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editing, setEditing] = useState<WikiArticle | null>(null)
  const [searchQuery, setSearchQuery] = useState("")
  const [categoryFilter, setCategoryFilter] = useState<string | null>(null)

  const { data: articles = [], isLoading } = useQuery({
    queryKey: ["wiki", categoryFilter],
    queryFn: () => wikiApi.list({ category: categoryFilter ?? undefined }),
  })

  const { data: categories = [] } = useQuery({
    queryKey: ["wiki-categories"],
    queryFn: () => wikiApi.categories(),
  })

  const { data: searchResults } = useQuery({
    queryKey: ["wiki-search", searchQuery],
    queryFn: () => wikiApi.search(searchQuery),
    enabled: searchQuery.length >= 2,
  })

  const createMutation = useMutation({
    mutationFn: (body: { title: string; content: string; category: string; tags: string[] }) =>
      wikiApi.create(body),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["wiki"] })
      toast.success("Article created")
      setDialogOpen(false)
    },
    onError: (e) => toast.error("Failed to create article", { description: String(e) }),
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
    onError: (e) => toast.error("Failed to update article", { description: String(e) }),
  })

  const deleteMutation = useMutation({
    mutationFn: (id: number) => wikiApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["wiki"] })
      toast.success("Article deleted")
    },
  })

  const pinMutation = useMutation({
    mutationFn: ({ id, is_pinned }: { id: number; is_pinned: boolean }) =>
      wikiApi.update(id, { is_pinned }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["wiki"] }),
  })

  const displayArticles = searchQuery.length >= 2 ? (searchResults ?? []) : articles

  return (
    <div className="mx-auto max-w-4xl space-y-6 p-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <BookOpen className="h-6 w-6 text-primary" />
          <h1 className="text-2xl font-bold">Knowledge Base</h1>
        </div>
        <Button onClick={() => { setEditing(null); setDialogOpen(true) }}>
          <Plus className="mr-1 h-4 w-4" /> New Article
        </Button>
      </div>

      {/* Search + Category filter */}
      <div className="flex gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" />
          <Input
            placeholder="Search articles..."
            className="pl-9"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
        </div>
        <select
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
        <div className="flex justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </div>
      ) : displayArticles.length === 0 ? (
        <p className="py-12 text-center text-muted-foreground">
          {searchQuery ? "No articles match your search." : "No articles yet. Create your first one!"}
        </p>
      ) : (
        <div className="space-y-3">
          {displayArticles.map((article) => (
            <Card key={article.id}>
              <CardHeader className="pb-2">
                <div className="flex items-start justify-between">
                  <div>
                    <CardTitle className="text-base">
                      {article.is_pinned && <Pin className="mr-1 inline h-3.5 w-3.5 text-yellow-500" />}
                      {article.title}
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
                    <Button variant="ghost" size="sm" onClick={() => pinMutation.mutate({ id: article.id, is_pinned: !article.is_pinned })}>
                      <Pin className="h-3.5 w-3.5" />
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => { setEditing(article); setDialogOpen(true) }}>
                      <Pencil className="h-3.5 w-3.5" />
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => deleteMutation.mutate(article.id)}>
                      <Trash2 className="h-3.5 w-3.5 text-red-500" />
                    </Button>
                  </div>
                </div>
              </CardHeader>
              <CardContent>
                <p className="line-clamp-2 text-sm text-muted-foreground">
                  {article.content.slice(0, 200) || "(empty)"}
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
    </div>
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
  const [title, setTitle] = useState(article?.title ?? "")
  const [content, setContent] = useState(article?.content ?? "")
  const [category, setCategory] = useState(article?.category ?? "general")
  const [tagsStr, setTagsStr] = useState(article?.tags.join(", ") ?? "")

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
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{article ? "Edit Article" : "New Article"}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label>Title</Label>
            <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Article title" />
          </div>
          <div>
            <Label>Content (Markdown)</Label>
            <Textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              rows={10}
              placeholder="Write your article in Markdown..."
            />
          </div>
          <div className="flex gap-3">
            <div className="flex-1">
              <Label>Category</Label>
              <Input value={category} onChange={(e) => setCategory(e.target.value)} />
            </div>
            <div className="flex-1">
              <Label>Tags (comma-separated)</Label>
              <Input value={tagsStr} onChange={(e) => setTagsStr(e.target.value)} placeholder="tag1, tag2" />
            </div>
          </div>
          <Button onClick={handleSubmit} className="w-full">
            {article ? "Save Changes" : "Create Article"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
