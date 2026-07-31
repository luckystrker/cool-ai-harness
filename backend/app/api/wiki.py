"""Wiki / Knowledge Base API routes (Фаза 3a §3).

  GET    /wiki                — list articles (with filters)
  GET    /wiki/search         — full-text search
  GET    /wiki/categories     — list categories
  GET    /wiki/stats          — dashboard stats
  GET    /wiki/{id}           — article detail
  POST   /wiki                — create article
  PATCH  /wiki/{id}           — update article
  DELETE /wiki/{id}           — delete article
  POST   /wiki/promote        — promote memory item to wiki
"""

from __future__ import annotations

from datetime import datetime

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field
from sqlmodel import Session

from app.core.db import get_session
from app.wiki import (
    create_article,
    delete_article,
    get_article,
    get_categories,
    get_stats,
    list_articles,
    promote_from_memory,
    search_articles,
    update_article,
)

router = APIRouter(prefix="/wiki", tags=["wiki"])


# --- Schemas ---


class WikiArticleOut(BaseModel):
    id: int
    title: str
    content: str
    category: str
    tags: list[str] = []
    source: str
    source_memory_id: int | None = None
    is_pinned: bool = False
    is_archived: bool = False
    version: int = 1
    created_at: datetime
    updated_at: datetime


class WikiArticleCreate(BaseModel):
    title: str = Field(..., min_length=1, max_length=200)
    content: str = ""
    category: str = "general"
    tags: list[str] = Field(default_factory=list)


class WikiArticleUpdate(BaseModel):
    title: str | None = None
    content: str | None = None
    category: str | None = None
    tags: list[str] | None = None
    is_pinned: bool | None = None
    is_archived: bool | None = None


class WikiPromoteRequest(BaseModel):
    memory_item_id: int
    title: str
    content: str
    category: str = "facts"
    tags: list[str] = Field(default_factory=lambda: ["from-memory"])


# --- Helpers ---


def _article_to_out(a) -> WikiArticleOut:
    return WikiArticleOut(
        id=a.id,
        title=a.title,
        content=a.content,
        category=a.category,
        tags=a.tags or [],
        source=a.source,
        source_memory_id=a.source_memory_id,
        is_pinned=a.is_pinned,
        is_archived=a.is_archived,
        version=a.version,
        created_at=a.created_at,
        updated_at=a.updated_at,
    )


# --- Routes ---


@router.get("", response_model=list[WikiArticleOut])
def get_wiki_articles(
    category: str | None = Query(default=None),
    tag: str | None = Query(default=None),
    include_archived: bool = Query(default=False),
    limit: int = Query(default=100, le=500),
    offset: int = Query(default=0, ge=0),
    session: Session = Depends(get_session),
) -> list[WikiArticleOut]:
    articles = list_articles(
        session,
        category=category,
        tag=tag,
        include_archived=include_archived,
        limit=limit,
        offset=offset,
    )
    return [_article_to_out(a) for a in articles]


@router.get("/search", response_model=list[WikiArticleOut])
def get_wiki_search(
    q: str = Query(..., min_length=1),
    limit: int = Query(default=20, le=100),
    session: Session = Depends(get_session),
) -> list[WikiArticleOut]:
    results = search_articles(session, q, limit=limit)
    return [_article_to_out(a) for a in results]


@router.get("/categories")
def get_wiki_categories(session: Session = Depends(get_session)) -> list[str]:
    return get_categories(session)


@router.get("/stats")
def get_wiki_stats(session: Session = Depends(get_session)) -> dict:
    return get_stats(session)


@router.get("/{article_id}", response_model=WikiArticleOut)
def get_wiki_article(article_id: int, session: Session = Depends(get_session)) -> WikiArticleOut:
    article = get_article(session, article_id)
    if article is None:
        raise HTTPException(status_code=404, detail="Article not found")
    return _article_to_out(article)


@router.post("", response_model=WikiArticleOut, status_code=201)
def post_wiki_article(
    body: WikiArticleCreate, session: Session = Depends(get_session)
) -> WikiArticleOut:
    article = create_article(
        session,
        title=body.title,
        content=body.content,
        category=body.category,
        tags=body.tags,
    )
    return _article_to_out(article)


@router.patch("/{article_id}", response_model=WikiArticleOut)
def patch_wiki_article(
    article_id: int, body: WikiArticleUpdate, session: Session = Depends(get_session)
) -> WikiArticleOut:
    article = update_article(
        session,
        article_id,
        title=body.title,
        content=body.content,
        category=body.category,
        tags=body.tags,
        is_pinned=body.is_pinned,
        is_archived=body.is_archived,
    )
    if article is None:
        raise HTTPException(status_code=404, detail="Article not found")
    return _article_to_out(article)


@router.delete("/{article_id}")
def delete_wiki_article(article_id: int, session: Session = Depends(get_session)) -> dict:
    if not delete_article(session, article_id):
        raise HTTPException(status_code=404, detail="Article not found")
    return {"deleted": article_id}


@router.post("/promote", response_model=WikiArticleOut, status_code=201)
def post_wiki_promote(
    body: WikiPromoteRequest, session: Session = Depends(get_session)
) -> WikiArticleOut:
    article = promote_from_memory(
        session,
        memory_item_id=body.memory_item_id,
        title=body.title,
        content=body.content,
        category=body.category,
        tags=body.tags,
    )
    return _article_to_out(article)
