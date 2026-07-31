"""Wiki / Knowledge Base service (Фаза 3a §3).

Provides CRUD, full-text search, and memory-promotion for wiki articles.
Articles are organized by category and tags, support Markdown content,
and are scoped by project_key for multi-project visibility.
"""

from __future__ import annotations

from collections.abc import Sequence

from sqlmodel import Session, col, select

from app.core.logging import get_logger
from app.models.wiki import WikiArticle

log = get_logger(__name__)


def create_article(
    session: Session,
    *,
    title: str,
    content: str = "",
    category: str = "general",
    tags: list[str] | None = None,
    source: str = "manual",
    source_memory_id: int | None = None,
    user_id: int | None = None,
    project_key: str | None = None,
) -> WikiArticle:
    """Create a new wiki article."""
    article = WikiArticle(
        title=title,
        content=content,
        category=category,
        tags=tags or [],
        source=source,
        source_memory_id=source_memory_id,
        user_id=user_id,
        project_key=project_key,
    )
    session.add(article)
    session.commit()
    session.refresh(article)
    log.info("wiki.article_created", id=article.id, title=title, category=category)
    return article


def get_article(session: Session, article_id: int) -> WikiArticle | None:
    """Get a single article by ID."""
    return session.get(WikiArticle, article_id)


def list_articles(
    session: Session,
    *,
    category: str | None = None,
    tag: str | None = None,
    project_key: str | None = None,
    include_archived: bool = False,
    limit: int = 100,
    offset: int = 0,
) -> Sequence[WikiArticle]:
    """List articles with optional filters."""
    stmt = select(WikiArticle).order_by(col(WikiArticle.updated_at).desc())  # type: ignore[union-attr]
    if not include_archived:
        stmt = stmt.where(WikiArticle.is_archived == False)  # noqa: E712
    if category:
        stmt = stmt.where(WikiArticle.category == category)
    if project_key:
        stmt = stmt.where(WikiArticle.project_key == project_key)
    if tag:
        # JSON array contains check — SQLite json_each approach is complex;
        # use a simple LIKE for now (tags stored as JSON array string).
        stmt = stmt.where(col(WikiArticle.tags).contains(tag))  # type: ignore[union-attr]
    stmt = stmt.limit(limit).offset(offset)
    return session.exec(stmt).all()


def search_articles(
    session: Session,
    query: str,
    *,
    project_key: str | None = None,
    limit: int = 20,
) -> Sequence[WikiArticle]:
    """Full-text search across article titles and content.

    Uses SQLite LIKE for simplicity (FTS5 can be added later for better
    ranking, similar to the memory subsystem).
    """
    stmt = (
        select(WikiArticle)
        .where(WikiArticle.is_archived == False)  # noqa: E712
        .where(
            col(WikiArticle.title).contains(query)  # type: ignore[union-attr]
            | col(WikiArticle.content).contains(query)  # type: ignore[union-attr]
        )
        .order_by(col(WikiArticle.updated_at).desc())  # type: ignore[union-attr]
        .limit(limit)
    )
    if project_key:
        stmt = stmt.where(WikiArticle.project_key == project_key)
    return session.exec(stmt).all()


def update_article(
    session: Session,
    article_id: int,
    *,
    title: str | None = None,
    content: str | None = None,
    category: str | None = None,
    tags: list[str] | None = None,
    is_pinned: bool | None = None,
    is_archived: bool | None = None,
) -> WikiArticle | None:
    """Update an article's fields. Increments version on content change."""
    article = session.get(WikiArticle, article_id)
    if article is None:
        return None
    if title is not None:
        article.title = title
    if content is not None:
        article.content = content
        article.version += 1
    if category is not None:
        article.category = category
    if tags is not None:
        article.tags = tags
    if is_pinned is not None:
        article.is_pinned = is_pinned
    if is_archived is not None:
        article.is_archived = is_archived
    session.add(article)
    session.commit()
    session.refresh(article)
    return article


def delete_article(session: Session, article_id: int) -> bool:
    """Hard-delete an article. Returns False if not found."""
    article = session.get(WikiArticle, article_id)
    if article is None:
        return False
    session.delete(article)
    session.commit()
    log.info("wiki.article_deleted", id=article_id)
    return True


def promote_from_memory(
    session: Session,
    *,
    memory_item_id: int,
    title: str,
    content: str,
    category: str = "facts",
    tags: list[str] | None = None,
    user_id: int | None = None,
    project_key: str | None = None,
) -> WikiArticle:
    """Promote a confirmed memory item into the knowledge base.

    Creates a new article with source='memory_promotion' and links back
    to the original memory item.
    """
    return create_article(
        session,
        title=title,
        content=content,
        category=category,
        tags=tags or ["from-memory"],
        source="memory_promotion",
        source_memory_id=memory_item_id,
        user_id=user_id,
        project_key=project_key,
    )


def get_categories(session: Session) -> list[str]:
    """List all distinct categories."""
    rows = session.exec(
        select(WikiArticle.category).distinct().order_by(WikiArticle.category)  # type: ignore[union-attr]
    ).all()
    return list(rows)  # type: ignore[arg-type]


def get_stats(session: Session) -> dict:
    """Wiki statistics for the dashboard."""
    from sqlalchemy import func

    total = session.exec(select(func.count(WikiArticle.id))).one()
    by_category = session.exec(
        select(WikiArticle.category, func.count(WikiArticle.id)).group_by(WikiArticle.category)  # type: ignore[union-attr]
    ).all()
    return {
        "total_articles": total,
        "by_category": {row[0]: row[1] for row in by_category},  # type: ignore[index]
    }
