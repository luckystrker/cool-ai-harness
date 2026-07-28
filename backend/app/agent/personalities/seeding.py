"""Idempotent seeder for built-in agent profiles (Фаза 3a §2).

Called from ``init_db()`` / app lifespan to ensure the five preset profiles
exist. Skips slugs that already exist so user edits are preserved.
"""

from __future__ import annotations

from sqlmodel import Session

from app.agent.personalities.presets import BUILTIN_PRESETS
from app.agent.personalities.service import create_profile, get_profile_by_slug
from app.core.logging import get_logger

log = get_logger(__name__)


def seed_builtin_profiles(session: Session) -> int:
    """Create any missing built-in profiles. Returns the count of newly created rows."""
    created = 0
    for preset in BUILTIN_PRESETS:
        existing = get_profile_by_slug(session, preset["slug"])
        if existing is not None:
            continue
        create_profile(
            session,
            name=preset["name"],
            slug=preset["slug"],
            description=preset.get("description"),
            system_prompt=preset.get("system_prompt"),
            model=preset.get("model"),
            tool_names=preset.get("tool_names"),
            skill_names=preset.get("skill_names"),
            settings=preset.get("settings"),
            avatar_color=preset.get("avatar_color"),
            is_builtin=True,
            is_active=True,
        )
        created += 1
    if created:
        log.info("profiles.seeded", count=created)
    return created
