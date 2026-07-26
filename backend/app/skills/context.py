"""Skills context injection for the agent system prompt (Фаза 2 §3).

Builds a context block that informs the agent about available skills and
suggests relevant ones based on the user's input. This is injected into the
system prompt so the agent knows it can activate skills via the ``use_skill``
tool without having to discover them manually every turn.
"""

from __future__ import annotations

from app.core.logging import get_logger
from app.skills.matching import select_relevant_skills
from app.skills.registry import get_skill_registry

log = get_logger(__name__)

# Maximum number of skills to suggest in the context block.
_MAX_SUGGESTED = 3


def build_skills_context(user_input: str) -> str | None:
    """Build a skills context string for system-prompt injection.

    Analyzes the user's input and selects relevant skills. Returns a formatted
    context block listing available skills with relevance indicators, or None
    if no skills are loaded.
    """
    registry = get_skill_registry()
    all_skills = registry.list_all()
    if not all_skills:
        return None

    # Determine which skills are relevant to this input.
    relevant = select_relevant_skills(user_input, all_skills, max_skills=_MAX_SUGGESTED)
    relevant_names = {s.name for s in relevant}

    lines = ["## Available Skills"]
    lines.append(
        "Skills are specialized instruction sets that guide your approach. "
        "Use the `use_skill` tool to activate one."
    )
    lines.append("")

    for skill in all_skills:
        marker = " **(suggested)**" if skill.name in relevant_names else ""
        lines.append(f"- `{skill.name}`: {skill.description}{marker}")

    if relevant:
        lines.append("")
        lines.append(
            f"Suggested for this task: {', '.join(f'`{s.name}`' for s in relevant)}. "
            "Activate a skill with `use_skill` to get detailed instructions."
        )

    return "\n".join(lines)
