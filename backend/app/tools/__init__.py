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
    from app.tools.code_tools import register_code_tools
    from app.tools.file_tools import register_file_tools
    from app.tools.web_tools import register_web_tools

    register_file_tools()
    register_code_tools()
    register_web_tools()


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
