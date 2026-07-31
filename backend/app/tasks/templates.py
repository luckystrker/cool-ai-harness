"""Built-in recurring workflow templates (Фаза 3b §5).

Each template is a ready-made ``ScheduledTask`` draft: a prompt, a sensible
default cron, a tool whitelist and limits. The API exposes them so the UI can
prefill the create form, and the agent-facing ``create_task`` tool accepts a
template slug so "set up my daily news digest" is a one-shot request.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class TaskTemplate:
    """A recurring-workflow preset."""

    slug: str
    name: str
    description: str
    prompt: str
    cron_expression: str
    tools_whitelist: list[str] | None = None
    max_iterations: int = 10
    delivery_channels: list[str] = field(default_factory=lambda: ["ui"])

    def to_dict(self) -> dict[str, Any]:
        return {
            "slug": self.slug,
            "name": self.name,
            "description": self.description,
            "prompt": self.prompt,
            "cron_expression": self.cron_expression,
            "tools_whitelist": self.tools_whitelist,
            "max_iterations": self.max_iterations,
            "delivery_channels": list(self.delivery_channels),
        }


TASK_TEMPLATES: tuple[TaskTemplate, ...] = (
    TaskTemplate(
        slug="news-digest",
        name="Daily news / research digest",
        description="Search the web for updates on your topics and produce a short digest.",
        prompt=(
            "Prepare a concise daily digest of notable news and research on my "
            "topics of interest. Search the web, group findings by theme, keep "
            "each item to one or two sentences, and include source links."
        ),
        cron_expression="0 8 * * *",
        tools_whitelist=["web_search", "web_fetch", "memory_recall"],
        max_iterations=12,
    ),
    TaskTemplate(
        slug="code-review",
        name="Code review / cleanup",
        description="Review recent changes in the working directory and report issues.",
        prompt=(
            "Review the code in my working directory. Look for bugs, dead code, "
            "missing error handling and style violations. Report the findings "
            "grouped by file, most important first, with concrete suggestions."
        ),
        cron_expression="0 18 * * 1-5",
        tools_whitelist=["read_file", "list_files"],
        max_iterations=15,
    ),
    TaskTemplate(
        slug="memory-review",
        name="Memory review",
        description="Periodically revisit long-term memory: stale, duplicate or unconfirmed items.",
        prompt=(
            "Review my long-term memory. Recall the most important stored items, "
            "point out anything stale, duplicated or contradictory, and suggest "
            "what should be updated or forgotten. Do not delete anything yourself."
        ),
        cron_expression="0 9 * * 1",
        tools_whitelist=["memory_recall", "memory_list"],
        max_iterations=8,
    ),
    TaskTemplate(
        slug="health-check",
        name="Health check / monitoring",
        description="Probe the configured endpoints and report anything unhealthy.",
        prompt=(
            "Check that my monitored endpoints respond correctly. Report status, "
            "latency and any failures. Keep the report to a few lines when "
            "everything is healthy."
        ),
        cron_expression="0 */6 * * *",
        tools_whitelist=["web_fetch"],
        max_iterations=6,
    ),
)


def get_template(slug: str) -> TaskTemplate | None:
    """Look up a template by slug."""
    for template in TASK_TEMPLATES:
        if template.slug == slug:
            return template
    return None


def list_templates() -> list[dict[str, Any]]:
    """All templates as plain dicts (API/tool friendly)."""
    return [t.to_dict() for t in TASK_TEMPLATES]
