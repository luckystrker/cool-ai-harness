"""Multi-personality agents (Фаза 3a §2).

Provides agent profile CRUD, built-in presets, and seeding logic.
"""

from app.agent.personalities.seeding import seed_builtin_profiles
from app.agent.personalities.service import (
    create_profile,
    delete_profile,
    get_profile,
    get_profile_by_slug,
    list_profiles,
    update_profile,
)

__all__ = [
    "create_profile",
    "delete_profile",
    "get_profile",
    "get_profile_by_slug",
    "list_profiles",
    "seed_builtin_profiles",
    "update_profile",
]
