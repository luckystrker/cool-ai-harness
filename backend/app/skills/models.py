"""Skill data model (Фаза 2 §3).

A Skill is a directory containing a ``SKILL.md`` file plus optional
scripts/resources. The SKILL.md front-matter (YAML between ``---`` fences)
carries structured metadata; the body is the skill's instruction prompt that
gets injected into the agent context when the skill is activated.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# --- SKILL.md parsing ---

_FRONT_MATTER_RE = re.compile(r"\A---\s*\n(.*?)\n---\s*\n", re.DOTALL)


def parse_skill_md(text: str) -> tuple[dict[str, Any], str]:
    """Parse a SKILL.md file into (metadata, body).

    Metadata is extracted from YAML front-matter (``---`` fenced block at the
    top). The body is the remaining markdown content (the skill's instruction
    prompt). If no front-matter is present, metadata is empty.
    """
    match = _FRONT_MATTER_RE.match(text)
    if not match:
        return {}, text.strip()

    raw_yaml = match.group(1)
    body = text[match.end():].strip()

    # Lightweight YAML-subset parser (avoids a PyYAML dependency for simple
    # key: value pairs and lists). Handles strings, lists (comma-separated or
    # YAML ``- item`` syntax), and booleans.
    metadata: dict[str, Any] = {}
    current_key: str | None = None
    for line in raw_yaml.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        # List item under current key.
        if stripped.startswith("- ") and current_key:
            if not isinstance(metadata.get(current_key), list):
                metadata[current_key] = []
            metadata[current_key].append(_parse_scalar(stripped[2:].strip()))
            continue
        # key: value
        if ":" in stripped:
            key, _, value = stripped.partition(":")
            key = key.strip()
            value = value.strip()
            current_key = key
            if value:
                metadata[key] = _parse_scalar(value)
            else:
                # Value may follow as list items.
                metadata[key] = None
    return metadata, body


def _parse_scalar(value: str) -> Any:
    """Parse a YAML scalar: booleans, numbers, quoted strings, or bare strings."""
    if value.lower() in ("true", "yes"):
        return True
    if value.lower() in ("false", "no"):
        return False
    # Quoted string.
    if (value.startswith('"') and value.endswith('"')) or (
        value.startswith("'") and value.endswith("'")
    ):
        return value[1:-1]
    # Comma-separated list inline.
    if "," in value:
        return [v.strip().strip("'\"") for v in value.split(",")]
    # Integer.
    try:
        return int(value)
    except ValueError:
        pass
    return value


# --- Skill dataclass ---


@dataclass
class Skill:
    """A loaded skill: metadata + instruction body + filesystem location.

    Attributes:
        name: Unique skill identifier (directory name or front-matter ``name``).
        description: Short human-readable description (from front-matter).
        body: The full instruction prompt (SKILL.md body after front-matter).
        path: Filesystem path to the skill directory.
        source: Where the skill was loaded from: "builtin", "user", or "plugin".
        tags: Keywords for relevance matching.
        tools: Tool names the skill recommends/uses.
        version: Optional version string.
        metadata: Full parsed front-matter dict (for extensibility).
    """

    name: str
    description: str
    body: str
    path: Path
    source: str = "builtin"
    tags: list[str] = field(default_factory=list)
    tools: list[str] = field(default_factory=list)
    version: str = "1.0"
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_directory(cls, skill_dir: Path, *, source: str = "builtin") -> Skill | None:
        """Load a skill from a directory containing SKILL.md.

        Returns None if the directory doesn't contain a valid SKILL.md.
        """
        skill_md = skill_dir / "SKILL.md"
        if not skill_md.is_file():
            return None
        try:
            text = skill_md.read_text(encoding="utf-8")
        except OSError:
            return None

        meta, body = parse_skill_md(text)
        name = meta.get("name", skill_dir.name)
        if not isinstance(name, str):
            name = skill_dir.name

        description = meta.get("description", "")
        if not isinstance(description, str):
            description = str(description)

        tags = meta.get("tags", [])
        if isinstance(tags, str):
            tags = [tags]
        elif not isinstance(tags, list):
            tags = []

        tools = meta.get("tools", [])
        if isinstance(tools, str):
            tools = [tools]
        elif not isinstance(tools, list):
            tools = []

        version = meta.get("version", "1.0")
        if not isinstance(version, str):
            version = str(version)

        return cls(
            name=name,
            description=description,
            body=body,
            path=skill_dir,
            source=source,
            tags=[str(t) for t in tags],
            tools=[str(t) for t in tools],
            version=version,
            metadata=meta,
        )

    def context_block(self) -> str:
        """Return the skill's instruction body formatted for system-prompt injection."""
        header = f"# Skill: {self.name}"
        if self.description:
            header += f" — {self.description}"
        return f"{header}\n\n{self.body}"

    def list_resources(self) -> list[Path]:
        """List optional resource files in the skill directory (excluding SKILL.md)."""
        if not self.path.is_dir():
            return []
        return [
            p
            for p in sorted(self.path.iterdir())
            if p.is_file() and p.name != "SKILL.md" and not p.name.startswith(".")
        ]
