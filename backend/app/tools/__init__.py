"""Built-in tools and the global registry.

Importing this package registers the bundled tools (files, code, web) on the
module-level registry. Custom / user tools are registered the same way via
``register_tool(...)``.
"""

from __future__ import annotations

from app.tools.base import (
    Tool,
    ToolArgs,
    ToolFunc,
    ToolResult,
    clear_registry,
    get_registry,
    get_tool,
    register_tool,
)


def register_builtins() -> None:
    """Register all bundled tools. Idempotent — last registration wins."""
    from app.memory.tools import register_memory_tools
    from app.tools.bash_tools import register_bash_tools
    from app.tools.code_tools import register_code_tools
    from app.tools.file_tools import register_file_tools
    from app.tools.git_tools import register_git_tools
    from app.tools.github_tools import register_github_tools
    from app.tools.mcp_tools import register_mcp_management_tools
    from app.tools.plan_tools import register_plan_tools
    from app.tools.research_tools import register_research_tools
    from app.tools.rss_tools import register_rss_tools
    from app.tools.skill_tools import register_skill_tools
    from app.tools.subagent_tools import register_subagent_tools
    from app.tools.task_tools import register_task_tools
    from app.tools.web_tools import register_web_tools
    from app.tools.wiki_tools import register_wiki_tools

    register_file_tools()
    register_code_tools()
    register_bash_tools()
    register_web_tools()
    register_git_tools()
    register_github_tools()
    register_plan_tools()
    register_skill_tools()
    register_mcp_management_tools()
    register_subagent_tools()
    register_memory_tools()
    register_task_tools()
    register_rss_tools()
    register_research_tools()
    register_wiki_tools()


# Auto-register on import so the agent loop sees them out of the box.
register_builtins()


__all__ = [
    "Tool",
    "ToolArgs",
    "ToolFunc",
    "ToolResult",
    "clear_registry",
    "get_registry",
    "get_tool",
    "register_builtins",
    "register_tool",
]
