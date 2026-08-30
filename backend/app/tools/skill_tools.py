"""Skill tools (Фаза 2 §3).

Provides tools that let the agent interact with the skills system:
- ``list_skills``: discover available skills.
- ``use_skill``: activate a skill by name, returning its instruction prompt.
- ``create_skill``: create a new skill from the agent loop.

These tools are registered alongside the builtin tools so the agent can
autonomously discover, activate, and create skills during a conversation.
"""

from __future__ import annotations

import re
from pathlib import Path

from pydantic import Field

from app.core.config import get_settings
from app.core.logging import get_logger
from app.skills.registry import get_skill_registry
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context

log = get_logger(__name__)

_SKILL_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


# --- list_skills ---


class ListSkillsArgs(ToolArgs):
    """Arguments for the list_skills tool."""

    source: str = Field(
        default="",
        description="Filter by source: 'builtin', 'user', 'plugin'. Empty = all.",
    )


async def _list_skills(source: str = "") -> ToolResult:
    """List available skills with their descriptions."""
    registry = get_skill_registry()

    skills = registry.list_by_source(source) if source else registry.list_all()
    allowed = get_run_context().skill_names
    if allowed is not None:
        skills = [skill for skill in skills if skill.name in set(allowed)]

    if not skills:
        return ToolResult.ok("No skills available." + (f" (source={source})" if source else ""))

    lines = [f"Available skills ({len(skills)}):"]
    for skill in skills:
        tags_str = f" [{', '.join(skill.tags)}]" if skill.tags else ""
        lines.append(f"- **{skill.name}** ({skill.source}): {skill.description}{tags_str}")

    return ToolResult.ok("\n".join(lines))


# --- use_skill ---


class UseSkillArgs(ToolArgs):
    """Arguments for the use_skill tool."""

    name: str = Field(description="Name of the skill to activate")


async def _use_skill(name: str) -> ToolResult:
    """Activate a skill by name, returning its full instruction prompt."""
    registry = get_skill_registry()
    skill = registry.get(name)
    allowed = get_run_context().skill_names
    if allowed is not None and name not in allowed:
        return ToolResult.err(f"Skill '{name}' is not enabled for this agent blueprint")

    if skill is None:
        available = registry.names()
        hint = f" Available skills: {', '.join(available)}" if available else ""
        return ToolResult.err(f"Skill '{name}' not found.{hint}")

    log.info("skill.activated", name=skill.name, source=skill.source)

    # Build the response: skill instructions + resource listing.
    parts = [skill.context_block()]

    resources = skill.list_resources()
    if resources:
        parts.append("\n## Resources")
        for res in resources:
            parts.append(f"- `{res.name}` ({res.parent.name}/{res.name})")

    if skill.tools:
        parts.append(f"\n## Recommended tools: {', '.join(skill.tools)}")

    return ToolResult.ok("\n".join(parts))


# --- create_skill ---


class CreateSkillArgs(ToolArgs):
    """Arguments for the create_skill tool."""

    name: str = Field(
        description="Skill name: lowercase alphanumeric with hyphens (e.g. 'my-skill')"
    )
    description: str = Field(default="", description="Short description of what the skill does")
    tags: list[str] = Field(default_factory=list, description="Keywords for relevance matching")
    tools: list[str] = Field(default_factory=list, description="Recommended tools for the skill")
    body: str = Field(description="Skill instruction content (markdown)")
    scope: str = Field(
        default="user",
        description="Where to create: 'global' (shared skills/) or 'user' (data/skills/)",
    )


async def _create_skill(
    name: str,
    description: str = "",
    tags: list[str] | None = None,
    tools: list[str] | None = None,
    body: str = "",
    scope: str = "user",
) -> ToolResult:
    """Create a new skill by writing a SKILL.md file."""
    tags = tags or []
    tools = tools or []

    # Validate name.
    if not _SKILL_NAME_RE.match(name):
        return ToolResult.err(
            f"Invalid skill name '{name}'. Must be lowercase alphanumeric with hyphens "
            "(e.g. 'my-skill', 'code-review')."
        )

    if not body.strip():
        return ToolResult.err("Skill body (instructions) cannot be empty.")

    if scope not in ("global", "user"):
        return ToolResult.err(f"Invalid scope '{scope}'. Must be 'global' or 'user'.")

    settings = get_settings()
    base_dir = (
        Path(settings.skills_dir) if scope == "global" else Path(settings.data_dir) / "skills"
    )
    skill_dir = base_dir / name

    if skill_dir.exists():
        return ToolResult.err(f"Skill '{name}' already exists at {skill_dir}.")

    # Build SKILL.md content.
    lines = ["---"]
    lines.append(f"name: {name}")
    if description:
        lines.append(f"description: {description}")
    lines.append('version: "1.0"')
    if tags:
        lines.append("tags:")
        for tag in tags:
            lines.append(f"  - {tag}")
    if tools:
        lines.append("tools:")
        for tool in tools:
            lines.append(f"  - {tool}")
    lines.append("---")
    lines.append("")
    lines.append(body.strip())
    lines.append("")

    content = "\n".join(lines)

    try:
        skill_dir.mkdir(parents=True, exist_ok=True)
        (skill_dir / "SKILL.md").write_text(content, encoding="utf-8")
    except OSError as exc:
        return ToolResult.err(f"Failed to create skill directory: {exc}")

    # Reload registry so the skill is immediately available.
    registry = get_skill_registry()
    registry.load(force=True)

    log.info("skill.created_via_tool", name=name, scope=scope, path=str(skill_dir))

    return ToolResult.ok(
        f"Skill '{name}' created successfully at {skill_dir}.\n"
        f"Scope: {scope} | Tags: {', '.join(tags) or 'none'}\n"
        "The skill is now available via use_skill."
    )


# --- Registration ---


def register_skill_tools() -> None:
    """Register skill tools. Idempotent."""
    register_tool(
        name="list_skills",
        description=(
            "List available skills. Skills are reusable AI capability modules "
            "that provide specialized instructions for tasks like research, "
            "coding, summarization, translation, and brainstorming."
        ),
        args_model=ListSkillsArgs,
        func=_list_skills,
    )
    register_tool(
        name="use_skill",
        description=(
            "Activate a skill by name to get specialized instructions for a task. "
            "The skill's instructions will guide your approach. "
            "Use list_skills first to see what's available."
        ),
        args_model=UseSkillArgs,
        func=_use_skill,
    )
    register_tool(
        name="create_skill",
        description=(
            "Create a new skill with a name, description, tags, and instruction body. "
            "Skills are stored as SKILL.md files and become immediately available. "
            "Use scope='global' for shared skills or scope='user' for personal ones."
        ),
        args_model=CreateSkillArgs,
        func=_create_skill,
    )
