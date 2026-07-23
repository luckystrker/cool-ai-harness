"""Agent loop: executor and events."""

from __future__ import annotations

from pathlib import Path

from app.agent.events import AgentEvent, EventKind
from app.agent.executor import AgentConfig, AgentExecutor, AgentLimits

__all__ = [
    "AgentConfig",
    "AgentEvent",
    "AgentExecutor",
    "AgentLimits",
    "EventKind",
    "get_default_system_prompt",
]

_DEFAULT_PROMPT_FILE = Path(__file__).parent / "default_system_prompt.txt"


def get_default_system_prompt() -> str:
    """Load the default system prompt.

    Resolution order:
    1. Settings.default_system_prompt (inline override from UI / env)
    2. Settings.system_prompt_file (custom file path)
    3. Built-in default_system_prompt.txt shipped with the package
    """
    from app.core.config import get_settings

    settings = get_settings()
    if settings.default_system_prompt:
        return settings.default_system_prompt
    if settings.system_prompt_file and settings.system_prompt_file.exists():
        return settings.system_prompt_file.read_text(encoding="utf-8")
    if _DEFAULT_PROMPT_FILE.exists():
        return _DEFAULT_PROMPT_FILE.read_text(encoding="utf-8")
    return ""
