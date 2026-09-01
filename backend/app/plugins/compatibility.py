"""Read-only adapters for documented Codex and Claude declarative bundles."""

from __future__ import annotations

import json
import os
import re
import stat
from pathlib import Path
from typing import Any, Literal

from app.plugins.loader import PLUGIN_SCHEMA_ID, PluginLoader
from app.plugins.models import (
    PluginBundle,
    PluginDiagnostic,
    PluginManifest,
    PluginProvenance,
)

_PLUGIN_NAME = re.compile(r"^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$")
_VENDOR_MANIFESTS: tuple[tuple[Literal["codex", "claude"], Path], ...] = (
    ("codex", Path(".codex-plugin/plugin.json")),
    ("claude", Path(".claude-plugin/plugin.json")),
)


class CompatibilityLoader:
    """Normalize declarative vendor layouts without executing their contents."""

    def __init__(self) -> None:
        self.portable = PluginLoader()

    def load(self, root: Path) -> PluginBundle:
        root = root.resolve()
        if (root / "plugin.json").is_file():
            return self.portable.load(root)
        for vendor, relative in _VENDOR_MANIFESTS:
            manifest_path = root / relative
            if (
                manifest_path.exists() or manifest_path.is_symlink()
            ) and not self._contained_vendor_manifest(root, manifest_path):
                return self._unsafe_manifest(root, vendor, manifest_path)
            if manifest_path.is_file():
                return self._load_vendor(root, vendor, manifest_path)
        if self._has_claude_defaults(root):
            return self._load_manifestless_claude(root)
        bundle = self.portable.load(root)
        bundle.diagnostics.insert(
            0,
            PluginDiagnostic(
                code="compat.format_unknown",
                message="no portable, Codex, or Claude manifest was found",
                level="blocker",
                status="unsafe",
                path=str(root),
            ),
        )
        return bundle

    def _unsafe_manifest(
        self, root: Path, vendor: Literal["codex", "claude"], path: Path
    ) -> PluginBundle:
        bundle = PluginBundle(
            root=root,
            manifest=None,
            provenance=PluginProvenance(
                source_type=vendor,
                source=str(root),
                content_hash=self.portable.content_hash(root),
            ),
        )
        self._diag(
            bundle,
            "compat.manifest_symlink",
            "vendor manifest must be a contained regular file without symlinks or reparse points",
            "blocker",
            path,
            "unsafe",
        )
        return bundle

    def _load_vendor(
        self,
        root: Path,
        vendor: Literal["codex", "claude"],
        manifest_path: Path,
    ) -> PluginBundle:
        bundle = PluginBundle(
            root=root,
            manifest=None,
            provenance=PluginProvenance(
                source_type=vendor,
                source=str(root),
                content_hash=self.portable.content_hash(root),
            ),
        )
        raw = self._read_manifest(bundle, manifest_path)
        if raw is None:
            return bundle
        return self._normalize_vendor(bundle, vendor, raw, manifest_path)

    def _load_manifestless_claude(self, root: Path) -> PluginBundle:
        bundle = PluginBundle(
            root=root,
            manifest=None,
            provenance=PluginProvenance(
                source_type="claude",
                source=str(root),
                content_hash=self.portable.content_hash(root),
            ),
        )
        return self._normalize_vendor(
            bundle,
            "claude",
            {"name": root.name},
            root / ".claude-plugin/plugin.json",
            manifestless=True,
        )

    def _normalize_vendor(
        self,
        bundle: PluginBundle,
        vendor: Literal["codex", "claude"],
        raw: dict[str, Any],
        manifest_path: Path,
        *,
        manifestless: bool = False,
    ) -> PluginBundle:
        name = raw.get("name")
        if not isinstance(name, str) or not _PLUGIN_NAME.fullmatch(name) or len(name) > 64:
            self._diag(
                bundle,
                "compat.manifest_invalid",
                "manifest name is not portable",
                "blocker",
                manifest_path,
                "unsafe",
            )
            return bundle
        bundle.manifest = PluginManifest(
            schema=PLUGIN_SCHEMA_ID,
            name=name,
            version=raw.get("version", "") if isinstance(raw.get("version", ""), str) else "",
            description=(
                raw.get("description", "") if isinstance(raw.get("description", ""), str) else ""
            ),
        )
        self._diag(
            bundle,
            f"compat.{vendor}.manifest",
            (
                f"{vendor.title()} default layout was normalized without an optional manifest"
                if manifestless
                else f"{vendor.title()} manifest was normalized to the canonical model"
            ),
            "info",
            manifest_path,
            "transformed",
        )
        configured_skills: Any = raw.get("skills")
        if vendor == "claude":
            skill_roots: list[str] = ["./skills"]
            if isinstance(configured_skills, str):
                skill_roots.append(configured_skills)
            elif isinstance(configured_skills, list):
                skill_roots.extend(configured_skills)
            elif configured_skills is not None:
                skill_roots = configured_skills
            if configured_skills is None and not (bundle.root / "skills").exists():
                skill_roots.append("./")
            self._load_skill_roots(bundle, skill_roots, manifest_path)
        else:
            self._load_skill_roots(bundle, configured_skills or "./skills", manifest_path)
        if vendor == "codex":
            self._codex_diagnostics(bundle, raw)
        else:
            self._claude_diagnostics(bundle, raw)
        return bundle

    def _load_skill_roots(self, bundle: PluginBundle, configured: Any, manifest_path: Path) -> None:
        values = [configured] if isinstance(configured, str) else configured
        if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
            self._diag(
                bundle,
                "compat.skills_invalid",
                "skills must be a path or list of paths",
                "error",
                manifest_path,
                "unsafe",
            )
            return
        for value in values:
            relative = value.removeprefix("./")
            skill_root = (bundle.root / relative).resolve()
            if not skill_root.is_relative_to(bundle.root):
                self._diag(
                    bundle,
                    "compat.skills_escape",
                    "skill root escapes the bundle",
                    "error",
                    manifest_path,
                    "unsafe",
                )
                continue
            if not skill_root.exists():
                continue
            if not skill_root.is_dir() or skill_root.is_symlink():
                self._diag(
                    bundle,
                    "compat.skills_invalid",
                    "skill root is not a contained directory",
                    "error",
                    manifest_path,
                    "unsafe",
                )
                continue
            direct_skill = skill_root / "SKILL.md"
            if direct_skill.is_file() and not direct_skill.is_symlink():
                skill = self.portable._load_skill(
                    bundle,
                    skill_root,
                    direct_skill,
                    enforce_directory_name=False,
                )
                if skill is not None and all(
                    existing.name != skill.name for existing in bundle.skills
                ):
                    skill.source = "plugin"
                    bundle.skills.append(skill)
            for skill_dir in sorted(skill_root.iterdir()):
                skill_file = skill_dir / "SKILL.md"
                if not skill_dir.is_dir() or skill_dir.is_symlink():
                    continue
                if not skill_file.is_file() or skill_file.is_symlink():
                    continue
                skill = self.portable._load_skill(bundle, skill_dir, skill_file)
                if skill is not None and all(
                    existing.name != skill.name for existing in bundle.skills
                ):
                    skill.source = "plugin"
                    bundle.skills.append(skill)
        if bundle.skills:
            self._diag(
                bundle,
                "compat.skills",
                "Agent Skills were loaded through canonical validation",
                "info",
                manifest_path,
                "transformed",
            )

    def _codex_diagnostics(self, bundle: PluginBundle, raw: dict[str, Any]) -> None:
        self._diagnose_path(
            bundle,
            ".mcp.json",
            "compat.codex.mcp",
            "MCP declaration requires canonical conversion before activation",
            "ignored",
        )
        self._diagnose_path(
            bundle,
            ".app.json",
            "compat.codex.app",
            "Codex app metadata is retained only as compatibility metadata",
            "ignored",
        )
        self._diagnose_path(
            bundle,
            "hooks/hooks.json",
            "compat.codex.hooks",
            "Codex hooks require a semantic adapter before activation",
            "ignored",
        )
        known = {"name", "version", "description", "skills"}
        for field in sorted(set(raw) - known):
            self._diag(
                bundle,
                "compat.codex.field",
                f"Codex manifest field is not activated: {field}",
                "warning",
                Path(".codex-plugin/plugin.json"),
                "ignored",
            )

    def _claude_diagnostics(self, bundle: PluginBundle, raw: dict[str, Any]) -> None:
        declarations = {
            "commands": "Claude commands require a command semantic adapter",
            "agents": "Claude agents require a role semantic adapter",
            "hooks": "Claude hooks require a hook semantic adapter",
            "mcpServers": "Claude MCP declarations require canonical conversion before activation",
            "lspServers": "LSP servers are a Tier-3 compatibility feature",
            "outputStyles": "Output styles are not part of the canonical runtime",
            "themes": "Themes are not part of the canonical runtime",
            "monitors": "Background monitors require a supervised compatibility adapter",
            "bin": "Plugin executables are not added to PATH by the canonical runtime",
            "settings": "Claude settings are not applied by the canonical runtime",
        }
        for field, message in declarations.items():
            if field in raw or self._claude_default_exists(bundle.root, field):
                self._diag(
                    bundle,
                    f"compat.claude.{field}",
                    message,
                    "warning",
                    Path(".claude-plugin/plugin.json"),
                    "ignored",
                )
        known = {"name", "version", "description", "skills", *declarations}
        for field in sorted(set(raw) - known):
            self._diag(
                bundle,
                "compat.claude.field",
                f"Claude manifest field is not activated: {field}",
                "warning",
                Path(".claude-plugin/plugin.json"),
                "ignored",
            )

    @staticmethod
    def _claude_default_exists(root: Path, field: str) -> bool:
        defaults = {
            "commands": root / "commands",
            "agents": root / "agents",
            "hooks": root / "hooks" / "hooks.json",
            "mcpServers": root / ".mcp.json",
            "lspServers": root / ".lsp.json",
            "outputStyles": root / "output-styles",
            "themes": root / "themes",
            "monitors": root / "monitors" / "monitors.json",
            "bin": root / "bin",
            "settings": root / "settings.json",
        }
        return defaults[field].exists()

    @staticmethod
    def _has_claude_defaults(root: Path) -> bool:
        candidates = (
            root / "SKILL.md",
            root / "skills",
            root / "commands",
            root / "agents",
            root / "hooks" / "hooks.json",
            root / ".mcp.json",
            root / ".lsp.json",
            root / "output-styles",
            root / "themes",
            root / "monitors" / "monitors.json",
            root / "bin",
            root / "settings.json",
        )
        return any(path.exists() for path in candidates)

    @classmethod
    def _contained_vendor_manifest(cls, root: Path, path: Path) -> bool:
        try:
            if not path.resolve().is_relative_to(root) or not path.is_file():
                return False
            relative = path.relative_to(root)
        except (OSError, ValueError):
            return False
        current = root
        for part in relative.parts:
            current /= part
            if cls._is_link_like(current):
                return False
        return True

    @staticmethod
    def _is_link_like(path: Path) -> bool:
        if path.is_symlink():
            return True
        if os.name != "nt":
            return False
        try:
            attributes = getattr(path.lstat(), "st_file_attributes", 0)
        except OSError:
            return False
        return bool(attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))

    def _diagnose_path(
        self,
        bundle: PluginBundle,
        relative: str,
        code: str,
        message: str,
        status: Literal["ignored", "unsafe"],
    ) -> None:
        if (bundle.root / relative).exists():
            self._diag(bundle, code, message, "warning", Path(relative), status)

    @staticmethod
    def _read_manifest(bundle: PluginBundle, path: Path) -> dict[str, Any] | None:
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            CompatibilityLoader._diag(
                bundle, "compat.manifest_unreadable", str(exc), "blocker", path, "unsafe"
            )
            return None
        if not isinstance(raw, dict):
            CompatibilityLoader._diag(
                bundle,
                "compat.manifest_invalid",
                "manifest must be an object",
                "blocker",
                path,
                "unsafe",
            )
            return None
        return raw

    @staticmethod
    def _diag(
        bundle: PluginBundle,
        code: str,
        message: str,
        level: Literal["info", "warning", "error", "blocker"],
        path: Path,
        status: Literal["supported", "transformed", "ignored", "unsafe"],
    ) -> None:
        try:
            display_path = str(path.resolve().relative_to(bundle.root))
        except (OSError, ValueError):
            display_path = str(path)
        bundle.diagnostics.append(
            PluginDiagnostic(
                code=code,
                message=message,
                level=level,
                status=status,
                path=display_path.replace("\\", "/"),
            )
        )
