"""Skills context injection for the agent system prompt (Фаза 2 §3).

Builds a context block that informs the agent about available skills and
suggests relevant ones based on the user's input. This is injected into the
system prompt so the agent knows it can activate skills via the ``use_skill``
tool without having to discover them manually every turn.

Two modes (controlled by ``settings.skills_context_mode``):
- ``relevant_only`` (default): list only the top-N matched skills + a count.
- ``all``: list the full catalog with relevance markers.
"""

from __future__ import annotations

from app.core.config import get_settings
from app.core.logging import get_logger
from app.skills.matching import select_relevant_skills
from app.skills.registry import get_skill_registry

log = get_logger(__name__)

# Maximum number of skills to suggest in the context block.
_MAX_SUGGESTED = 3


def build_skills_context(user_input: str) -> str | None:
    """Build a skills context string for system-prompt injection.

    Respects ``settings.skills_context_mode``:
    - ``"relevant_only"``: list only matched skills (max 3) + availability note.
    - ``"all"``: list the full catalog with **(suggested)** markers on matches.

    Returns None if no skills are loaded.
    """
    registry = get_skill_registry()
    all_skills = registry.list_all()
    if not all_skills:
        return None

    settings = get_settings()
    mode = settings.skills_context_mode

    # Determine which skills are relevant to this input.
    relevant = select_relevant_skills(user_input, all_skills, max_skills=_MAX_SUGGESTED)
    relevant_names = {s.name for s in relevant}

    if mode == "all":
        return _build_all_mode(all_skills, relevant_names)
    return _build_relevant_mode(all_skills, relevant, relevant_names)


def _build_all_mode(all_skills: list, relevant_names: set[str]) -> str:
    """Full catalog mode: list every skill with relevance markers."""
    lines = ["## Available Skills"]
    lines.append(
        "Skills are specialized instruction sets that guide your approach. "
        "Use the `use_skill` tool to activate one."
    )
    lines.append("")

    for skill in all_skills:
        marker = " **(suggested)**" if skill.name in relevant_names else ""
        lines.append(f"- `{skill.name}`: {skill.description}{marker}")

    if relevant_names:
        lines.append("")
        lines.append(
            f"Suggested for this task: {', '.join(f'`{n}`' for n in sorted(relevant_names))}. "
            "Activate a skill with `use_skill` to get detailed instructions."
        )

    return "\n".join(lines)


def _build_relevant_mode(all_skills: list, relevant: list, relevant_names: set[str]) -> str:
    """Relevant-only mode: list matched skills + availability note."""
    if not relevant:
        return (
            "## Available Skills\n"
            f"{len(all_skills)} skills are available. "
            "Use the `use_skill` tool to activate one by name if a task "
            "would benefit from structured guidance."
        )

    lines = ["## Available Skills"]
    lines.append(
        "Skills are specialized instruction sets that guide your approach. "
        "Use the `use_skill` tool to activate one."
    )
    lines.append("")

    for skill in relevant:
        lines.append(f"- `{skill.name}`: {skill.description} **(suggested)**")

    remaining = len(all_skills) - len(relevant)
    if remaining > 0:
        lines.append("")
        lines.append(
            f"{remaining} more skill{'s' if remaining != 1 else ''} available "
            "\u2014 use `use_skill` to activate any by name."
        )

    lines.append("")
    lines.append(
        f"Suggested for this task: {', '.join(f'`{s.name}`' for s in relevant)}. "
        "Activate a skill with `use_skill` to get detailed instructions."
    )

    return "\n".join(lines)
