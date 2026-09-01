"""SkillRegistry — discovers and loads skills from multiple directories (Фаза 2 §3).

Skills are loaded from three sources (in priority order):
1. **builtin** — shipped with the repository under ``skills/`` (repo root).
2. **user** — user-created skills under ``data/skills/`` (gitignored).
3. **plugin** — skills provided by enabled, integrity-checked plugins.

User skills override builtins. Plugins cannot shadow an existing skill; a
collision is diagnosed and the already-loaded skill wins.
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
            self._load_builtin_via_plugin_contract(builtin_dir, target=skills)

        # Also check the configured skills_dir if different from repo root.
        configured_dir = Path(settings.skills_dir)
        if configured_dir != builtin_dir and configured_dir.is_dir():
            self._load_builtin_via_plugin_contract(configured_dir, target=skills)

        # 2. User skills (data/skills/).
        user_dir = Path(settings.data_dir) / "skills"
        if user_dir.is_dir():
            self._load_from_dir(user_dir, source="user", target=skills)

        # 3. Enabled plugin skills. Integrity failures are isolated per bundle.
        self._load_plugin_skills(Path(settings.data_dir) / "plugins", target=skills)

        self._skills = skills
        self._loaded = True
        log.info("skills.loaded", count=len(skills), names=list(skills.keys()))

    def _load_builtin_via_plugin_contract(
        self, base_dir: Path, *, target: dict[str, Skill]
    ) -> None:
        # Lazy import avoids the existing mcp -> tools -> skills package cycle.
        from app.plugins.loader import PluginLoader

        bundle = PluginLoader().load_builtin_skills(base_dir)
        for diagnostic in bundle.diagnostics:
            log.warning(
                "skills.plugin_contract_diagnostic",
                code=diagnostic.code,
                path=diagnostic.path,
                message=diagnostic.message,
            )
        for skill in bundle.skills:
            target[skill.name] = skill
            log.debug("skills.loaded_one", name=skill.name, source="builtin", path=str(skill.path))

    def _load_from_dir(self, base_dir: Path, *, source: str, target: dict[str, Skill]) -> None:
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

    @staticmethod
    def _load_plugin_skills(base_dir: Path, *, target: dict[str, Skill]) -> None:
        from app.plugins.models import PluginDiagnostic
        from app.plugins.store import PluginStore, PluginStoreError

        try:
            bundles = PluginStore(base_dir).load_enabled()
        except PluginStoreError as exc:
            log.error("skills.plugin_store_invalid", error=str(exc))
            return
        for bundle in bundles:
            for diagnostic in bundle.diagnostics:
                log.warning(
                    "skills.plugin_diagnostic",
                    code=diagnostic.code,
                    path=diagnostic.path,
                    message=diagnostic.message,
                )
            if bundle.manifest is None:
                continue
            for skill in bundle.skills:
                if skill.name in target:
                    diagnostic = PluginDiagnostic(
                        code="registry.skill_collision",
                        message=f"plugin skill does not replace existing skill: {skill.name}",
                        level="error",
                        status="unsafe",
                        path=str(skill.path),
                    )
                    bundle.diagnostics.append(diagnostic)
                    log.error(
                        "skills.plugin_collision",
                        plugin=bundle.manifest.name,
                        name=skill.name,
                        path=str(skill.path),
                    )
                    continue
                skill.source = "plugin"
                target[skill.name] = skill
                log.debug(
                    "skills.loaded_one", name=skill.name, source="plugin", path=str(skill.path)
                )

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
