"""Wiki / Knowledge Base agent tools (Фаза 3a §3).

Registers tools that allow the agent to read, write, search, and update
wiki articles. The agent uses these to maintain organized documentation
for projects, campaigns, research, etc.
"""

from __future__ import annotations

from pydantic import Field

from app.tools.base import ToolArgs, ToolResult, register_tool


class ReadWikiArgs(ToolArgs):
    """Arguments for read_wiki."""

    article_id: int | None = Field(default=None, description="ID of the article to read.")
    title: str | None = Field(default=None, description="Title of the article (searched if no ID).")


class WriteWikiArgs(ToolArgs):
    """Arguments for write_wiki."""

    title: str = Field(description="Title for the new article.")
    content: str = Field(description="Markdown content of the article.")
    category: str = Field(default="general", description="Category (e.g. project, research, how-to).")
    tags: list[str] = Field(default_factory=list, description="Tags for organization.")


class SearchWikiArgs(ToolArgs):
    """Arguments for search_wiki."""

    query: str = Field(description="Search query (matches title and content).")
    category: str | None = Field(default=None, description="Filter by category.")
    limit: int = Field(default=10, description="Max results to return.")


class UpdateWikiArgs(ToolArgs):
    """Arguments for update_wiki."""

    article_id: int = Field(description="ID of the article to update.")
    title: str | None = Field(default=None, description="New title (optional).")
    content: str | None = Field(default=None, description="New content (optional).")
    category: str | None = Field(default=None, description="New category (optional).")
    tags: list[str] | None = Field(default=None, description="New tags (optional).")


async def _read_wiki(article_id: int | None = None, title: str | None = None) -> ToolResult:
    """Read a wiki article by ID or title."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.wiki import get_article, list_articles

    with Session(engine) as session:
        if article_id:
            article = get_article(session, article_id)
            if article is None:
                return ToolResult.err(f"Article #{article_id} not found.")
            return ToolResult.ok(
                f"# {article.title}\n\n"
                f"**Category:** {article.category} | **Tags:** {', '.join(article.tags)}\n\n"
                f"{article.content}"
            )
        elif title:
            articles = list_articles(session, limit=5)
            matches = [a for a in articles if title.lower() in a.title.lower()]
            if not matches:
                return ToolResult.err(f"No article found matching title '{title}'.")
            article = matches[0]
            return ToolResult.ok(
                f"# {article.title} (id={article.id})\n\n"
                f"**Category:** {article.category} | **Tags:** {', '.join(article.tags)}\n\n"
                f"{article.content}"
            )
        else:
            return ToolResult.err("Provide either article_id or title.")


async def _write_wiki(
    title: str, content: str, category: str = "general", tags: list[str] | None = None
) -> ToolResult:
    """Create a new wiki article."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.tools.context import get_run_context
    from app.wiki import create_article

    ctx = get_run_context()

    with Session(engine) as session:
        article = create_article(
            session,
            title=title,
            content=content,
            category=category,
            tags=tags or [],
            source="agent",
            project_key=ctx.project_key if hasattr(ctx, "project_key") else None,
        )
        return ToolResult.ok(
            f"Created article '{article.title}' (id={article.id}) in category '{article.category}'."
        )


async def _search_wiki(
    query: str, category: str | None = None, limit: int = 10
) -> ToolResult:
    """Search wiki articles."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.wiki import search_articles

    with Session(engine) as session:
        results = search_articles(session, query, limit=limit)
        if category:
            results = [a for a in results if a.category == category]
        if not results:
            return ToolResult.ok(f"No articles found matching '{query}'.")
        lines = [f"Found {len(results)} article(s) matching '{query}':\n"]
        for a in results:
            tags_str = f" [{', '.join(a.tags)}]" if a.tags else ""
            lines.append(f"- **{a.title}** (id={a.id}, {a.category}){tags_str}")
            # Show first 100 chars of content as preview.
            preview = a.content[:100].replace("\n", " ").strip()
            if preview:
                lines.append(f"  {preview}...")
        return ToolResult.ok("\n".join(lines))


async def _update_wiki(
    article_id: int,
    title: str | None = None,
    content: str | None = None,
    category: str | None = None,
    tags: list[str] | None = None,
) -> ToolResult:
    """Update an existing wiki article."""
    from sqlmodel import Session

    from app.core.db import engine
    from app.wiki import update_article

    with Session(engine) as session:
        article = update_article(
            session,
            article_id,
            title=title,
            content=content,
            category=category,
            tags=tags,
        )
        if article is None:
            return ToolResult.err(f"Article #{article_id} not found.")
        return ToolResult.ok(
            f"Updated article '{article.title}' (id={article.id}, version={article.version})."
        )


def register_wiki_tools() -> None:
    """Register wiki-related tools. Idempotent."""
    register_tool(
        name="read_wiki",
        description=(
            "Read a wiki/knowledge-base article by ID or title. "
            "Returns the full Markdown content with metadata."
        ),
        args_model=ReadWikiArgs,
        func=_read_wiki,
        dangerous=False,
    )
    register_tool(
        name="write_wiki",
        description=(
            "Create a new wiki/knowledge-base article with Markdown content. "
            "Use for documenting findings, project notes, campaign lore, etc."
        ),
        args_model=WriteWikiArgs,
        func=_write_wiki,
        dangerous=False,
    )
    register_tool(
        name="search_wiki",
        description=(
            "Search wiki articles by keyword. Matches against title and content. "
            "Optionally filter by category."
        ),
        args_model=SearchWikiArgs,
        func=_search_wiki,
        dangerous=False,
    )
    register_tool(
        name="update_wiki",
        description=(
            "Update an existing wiki article's title, content, category, or tags. "
            "Provide the article_id and the fields to change."
        ),
        args_model=UpdateWikiArgs,
        func=_update_wiki,
        dangerous=False,
    )
