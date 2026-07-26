"""SkillRegistry — discovers and loads skills from multiple directories (Фаза 2 §3).

Skills are loaded from three sources (in priority order):
1. **builtin** — shipped with the repository under ``skills/`` (repo root).
2. **user** — user-created skills under ``data/skills/`` (gitignored).
3. **plugin** — skills provided by installed plugins (future, Фаза 2 §2).

Later sources override earlier ones by name (user > builtin), allowing users
to customize or replace built-in skills without editing the repo.
"""

from __future__ import annotations

from pathlib import Path

from app.core.config import get_settings
from app.core.logging import get_logger
from app.skills.models import Skill

log = get_logger(__name__)

# Builtin skills are stored in the repo root ``skills/`` directory.
_BUILTIN_SKILLS_DIR = Path(__file__).resolve().parents[3] / "skills"


class SkillRegistry:
    """Registry that discovers, loads, and provides access to skills.

    Usage::

        registry = SkillRegistry()
        registry.load()
        skill = registry.get("deep-research")
        relevant = registry.match("research quantum computing trends")
    """

    def __init__(self) -> None:
        self._skills: dict[str, Skill] = {}
        self._loaded = False

    # --- Loading ---

    def load(self, *, force: bool = False) -> None:
        """Discover and load skills from all source directories.

        Idempotent unless ``force=True``. Skills are loaded in priority order
        (builtin → user → plugin); later sources override earlier by name.
        """
        if self._loaded and not force:
            return

        skills: dict[str, Skill] = {}
        settings = get_settings()

        # 1. Builtin skills (repo root / skills/).
        builtin_dir = _BUILTIN_SKILLS_DIR
        if builtin_dir.is_dir():
            self._load_from_dir(builtin_dir, source="builtin", target=skills)

        # Also check the configured skills_dir if different from repo root.
        configured_dir = Path(settings.skills_dir)
        if configured_dir != builtin_dir and configured_dir.is_dir():
            self._load_from_dir(configured_dir, source="builtin", target=skills)

        # 2. User skills (data/skills/).
        user_dir = Path(settings.data_dir) / "skills"
        if user_dir.is_dir():
            self._load_from_dir(user_dir, source="user", target=skills)

        # 3. Plugin skills (future — Фаза 2 §2 plugin lifecycle).
        # plugin_dir = Path(settings.data_dir) / "plugins" / "skills"
        # if plugin_dir.is_dir():
        #     self._load_from_dir(plugin_dir, source="plugin", target=skills)

        self._skills = skills
        self._loaded = True
        log.info("skills.loaded", count=len(skills), names=list(skills.keys()))

    def _load_from_dir(
        self, base_dir: Path, *, source: str, target: dict[str, Skill]
    ) -> None:
        """Load all skill subdirectories from a base directory."""
        for entry in sorted(base_dir.iterdir()):
            if not entry.is_dir():
                continue
            if entry.name.startswith((".", "_")):
                continue
            skill = Skill.from_directory(entry, source=source)
            if skill is not None:
                target[skill.name] = skill
                log.debug("skills.loaded_one", name=skill.name, source=source, path=str(entry))

    # --- Access ---

    def get(self, name: str) -> Skill | None:
        """Look up a skill by name. Returns None if not found."""
        self._ensure_loaded()
        return self._skills.get(name)

    def list_all(self) -> list[Skill]:
        """Return all loaded skills, sorted by name."""
        self._ensure_loaded()
        return sorted(self._skills.values(), key=lambda s: s.name)

    def list_by_source(self, source: str) -> list[Skill]:
        """Return skills from a specific source (builtin/user/plugin)."""
        self._ensure_loaded()
        return sorted(
            (s for s in self._skills.values() if s.source == source),
            key=lambda s: s.name,
        )

    def names(self) -> list[str]:
        """All registered skill names."""
        self._ensure_loaded()
        return sorted(self._skills.keys())

    @property
    def count(self) -> int:
        self._ensure_loaded()
        return len(self._skills)

    def _ensure_loaded(self) -> None:
        """Lazy-load on first access."""
        if not self._loaded:
            self.load()

    # --- Registration (programmatic) ---

    def register(self, skill: Skill) -> None:
        """Register a skill programmatically (e.g. from a plugin or test).

        Overrides any existing skill with the same name.
        """
        self._skills[skill.name] = skill

    def unregister(self, name: str) -> bool:
        """Remove a skill by name. Returns True if it existed."""
        return self._skills.pop(name, None) is not None

    def clear(self) -> None:
        """Remove all skills. Intended for tests."""
        self._skills.clear()
        self._loaded = False


# --- Module-level singleton ---

_registry: SkillRegistry | None = None


def get_skill_registry() -> SkillRegistry:
    """Return the global SkillRegistry singleton."""
    global _registry
    if _registry is None:
        _registry = SkillRegistry()
    return _registry


def reset_skill_registry() -> None:
    """Reset the global registry. Intended for tests."""
    global _registry
    _registry = None
