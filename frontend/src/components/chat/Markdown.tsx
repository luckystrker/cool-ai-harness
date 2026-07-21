import { memo } from "react"
import ReactMarkdown from "react-markdown"
import remarkGfm from "remark-gfm"
import rehypeHighlight from "rehype-highlight"
import { cn } from "@/lib/utils"

/**
 * Markdown renderer with GFM tables/tasklists and code-block syntax highlighting.
 * `highlight.js` theme is loaded via a side-effect import below; swap to a
 * different CSS theme (github-dark.css etc.) by changing the import.
 */
const MarkdownImpl = ({ content, className }: { content: string; className?: string }) => {
  return (
    <div
      className={cn(
        // Prose-ish spacing without depending on @tailwindcss/typography.
        "max-w-none space-y-3 text-sm leading-relaxed",
        "[&_p]:my-0 [&_p+p]:mt-3",
        "[&_ul]:list-disc [&_ul]:pl-5 [&_ul+p]:mt-3",
        "[&_ol]:list-decimal [&_ol]:pl-5",
        "[_li]:my-1",
        "[_h1]:text-lg [_h1]:font-semibold [_h1]:mt-3",
        "[_h2]:text-base [_h2]:font-semibold [_h2]:mt-3",
        "[_h3]:text-sm [_h3]:font-semibold [_h3]:mt-2",
        "[_a]:text-blue-600 [_a]:underline dark:[_a]:text-blue-400",
        "[_blockquote]:border-l-2 [_blockquote]:border-muted-foreground/40 [_blockquote]:pl-3 [_blockquote]:text-muted-foreground",
        "[_code]:rounded [_code]:bg-muted [_code]:px-1 [_code]:py-0.5 [_code]:text-[0.85em] [_code]:font-mono",
        "[_pre]:overflow-x-auto [_pre]:rounded-md [_pre]:bg-zinc-900 [_pre]:p-3 [_pre]:text-zinc-100",
        "[_pre_code]:bg-transparent [_pre_code]:p-0 [_pre_code]:text-inherit",
        "[_table]:border-collapse [_table]:w-full [_table]:text-xs",
        "[_th]:border [_th]:border-border [_th]:px-2 [_th]:py-1 [_th]:bg-muted",
        "[_td]:border [_td]:border-border [_td]:px-2 [_td]:py-1",
        className
      )}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]}>
        {content}
      </ReactMarkdown>
    </div>
  )
}

export const Markdown = memo(MarkdownImpl)
