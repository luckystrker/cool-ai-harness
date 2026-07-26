"""Skills system (Фаза 2 §3).

A Skill is a directory with a ``SKILL.md`` file plus optional scripts/resources.
The SkillRegistry discovers skills from builtin, user, and plugin directories.
Relevance matching selects which skills to inject into the agent context.
"""

from __future__ import annotations

from app.skills.matching import (
    SkillScore,
    keyword_score,
    rank_skills,
    select_relevant_skills,
)
from app.skills.models import Skill, parse_skill_md
from app.skills.registry import SkillRegistry, get_skill_registry, reset_skill_registry

__all__ = [
    "Skill",
    "SkillRegistry",
    "SkillScore",
    "get_skill_registry",
    "keyword_score",
    "parse_skill_md",
    "rank_skills",
    "reset_skill_registry",
    "select_relevant_skills",
]