"""Skill relevance matching (Фаза 2 §3).

Determines which skills are relevant to a user's task using a two-tier
approach:

1. **Keyword / tag overlap** — fast, deterministic. Compares tokens from the
   user query against skill names, descriptions, and tags using TF-IDF-like
   weighting. Always available (no external dependencies).

2. **Embedding similarity** (optional) — when an embedding provider is
   configured, computes cosine similarity between the query embedding and
   pre-computed skill embeddings for higher-quality semantic matching.

The final score is a weighted combination. When embeddings are unavailable,
keyword scoring alone drives selection.
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass

from app.core.logging import get_logger
from app.skills.models import Skill

log = get_logger(__name__)

# Minimum score threshold for a skill to be considered relevant.
DEFAULT_THRESHOLD = 0.15

# --- Tokenization ---

_STOP_WORDS = frozenset([
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "shall",
    "should", "may", "might", "can", "could", "of", "in", "to", "for",
    "on", "with", "at", "by", "from", "as", "into", "through", "during",
    "before", "after", "above", "below", "between", "out", "off", "over",
    "under", "again", "further", "then", "once", "here", "there", "all",
    "each", "every", "both", "few", "more", "most", "other", "some",
    "such", "no", "nor", "not", "only", "own", "same", "so", "than",
    "too", "very", "just", "because", "but", "and", "or", "if", "while",
    "about", "against", "this", "that", "these", "those", "it", "its",
    "i", "me", "my", "we", "our", "you", "your", "he", "him", "his",
    "she", "her", "they", "them", "their", "what", "which", "who", "whom",
])

_TOKEN_RE = re.compile(r"[a-z0-9]+(?:[-_][a-z0-9]+)*", re.IGNORECASE)


def tokenize(text: str) -> list[str]:
    """Extract normalized tokens from text, removing stop-words."""
    tokens = _TOKEN_RE.findall(text.lower())
    return [t for t in tokens if t not in _STOP_WORDS and len(t) > 1]


# --- Keyword scoring ---


def keyword_score(query: str, skill: Skill) -> float:
    """Compute a keyword-overlap relevance score between a query and a skill.

    Score is in [0, 1]. Higher = more relevant. Uses token overlap with
    boosting for matches in the skill name and tags (vs description/body).
    """
    query_tokens = set(tokenize(query))
    if not query_tokens:
        return 0.0

    # Build weighted token sets from the skill.
    name_tokens = set(tokenize(skill.name.replace("-", " ").replace("_", " ")))
    tag_tokens = set(tokenize(" ".join(skill.tags)))
    desc_tokens = set(tokenize(skill.description))
    # Only use first 200 tokens of body to keep scoring fast.
    body_tokens = set(tokenize(skill.body[:2000]))

    # Weighted overlap: name/tags matches are worth more than body matches.
    name_overlap = len(query_tokens & name_tokens)
    tag_overlap = len(query_tokens & tag_tokens)
    desc_overlap = len(query_tokens & desc_tokens)
    body_overlap = len(query_tokens & body_tokens)

    # Weighted score: name (3x), tags (2.5x), description (2x), body (1x).
    raw = (name_overlap * 3.0) + (tag_overlap * 2.5) + (desc_overlap * 2.0) + (body_overlap * 1.0)

    # Normalize by query length to keep scores comparable across queries.
    max_possible = len(query_tokens) * 3.0  # If every token matched in name.
    return min(raw / max_possible, 1.0) if max_possible > 0 else 0.0


# --- Embedding similarity (optional) ---


def cosine_similarity(a: list[float], b: list[float]) -> float:
    """Cosine similarity between two vectors. Returns 0.0 for zero vectors."""
    if len(a) != len(b) or not a:
        return 0.0
    dot = sum(x * y for x, y in zip(a, b, strict=True))
    norm_a = math.sqrt(sum(x * x for x in a))
    norm_b = math.sqrt(sum(x * x for x in b))
    if norm_a == 0 or norm_b == 0:
        return 0.0
    return dot / (norm_a * norm_b)


@dataclass
class SkillScore:
    """A skill with its computed relevance score."""

    skill: Skill
    score: float
    keyword_score: float
    embedding_score: float | None = None


def rank_skills(
    query: str,
    skills: list[Skill],
    *,
    threshold: float = DEFAULT_THRESHOLD,
    query_embedding: list[float] | None = None,
    skill_embeddings: dict[str, list[float]] | None = None,
    keyword_weight: float = 0.6,
    embedding_weight: float = 0.4,
) -> list[SkillScore]:
    """Rank skills by relevance to a query.

    Returns skills scoring above ``threshold``, sorted by score descending.
    When embedding data is provided, combines keyword and embedding scores
    using the given weights. Otherwise uses keyword scoring alone.
    """
    results: list[SkillScore] = []

    for skill in skills:
        kw = keyword_score(query, skill)
        emb: float | None = None

        if query_embedding and skill_embeddings and skill.name in skill_embeddings:
            emb = cosine_similarity(query_embedding, skill_embeddings[skill.name])
            # Clamp negative cosine to 0.
            emb = max(emb, 0.0)
            combined = (keyword_weight * kw) + (embedding_weight * emb)
        else:
            combined = kw

        if combined >= threshold:
            results.append(
                SkillScore(skill=skill, score=combined, keyword_score=kw, embedding_score=emb)
            )

    results.sort(key=lambda s: s.score, reverse=True)
    return results


def select_relevant_skills(
    query: str,
    skills: list[Skill],
    *,
    max_skills: int = 3,
    threshold: float = DEFAULT_THRESHOLD,
) -> list[Skill]:
    """Select the top-N relevant skills for a query (keyword-only, no embeddings).

    Convenience wrapper for the common case where embedding infrastructure
    isn't configured. Returns at most ``max_skills`` skills above threshold.
    """
    scored = rank_skills(query, skills, threshold=threshold)
    return [s.skill for s in scored[:max_skills]]
