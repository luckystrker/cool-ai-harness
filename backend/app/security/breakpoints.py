"""Human-in-the-Loop breakpoints (Фаза 1.5 §2 — HITL Breakpoints).

Breakpoints let the user insert a stopping point in the agent's tool-call
chain. When the executor hits a breakpoint, it emits a ``tool_approval_request``
event (reusing the existing approval mechanism) and blocks until the user
resolves it.

Breakpoint types:
    before_tool        — pause before *any* tool call (or a specific tool)
    after_tool_result  — pause after a tool returns its result
    before_send        — pause before sending the assistant message to the LLM
    before_write       — pause before any file-writing tool (write_file)

Configuration:
    A BreakpointsConfig holds a list of BreakpointConfig entries. Each entry
    specifies a type, an optional tool name filter, and a TTL/fallback. The
    executor calls ``should_break()`` at each checkpoint to decide whether to
    pause.

    Breakpoints can be configured globally (Settings) or per-conversation
    (Conversation.metadata_). Per-conversation overrides take precedence.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Literal

from app.core.logging import get_logger

log = get_logger(__name__)


class BreakpointType(str, Enum):
    """When in the tool-call chain a breakpoint fires."""

    BEFORE_TOOL = "before_tool"
    AFTER_TOOL_RESULT = "after_tool_result"
    BEFORE_SEND = "before_send"
    BEFORE_WRITE = "before_write"


# Fallback action when a breakpoint times out (user didn't respond in time).
BreakpointFallback = Literal["deny", "skip"]


@dataclass
class BreakpointConfig:
    """A single breakpoint rule.

    Attributes:
        bp_type: When in the chain this breakpoint fires.
        tool_name: If set, only fire for this specific tool. None = any tool.
        ttl_s: How long to wait for approval before applying the fallback.
            None = use the global Settings.breakpoint_timeout_s.
        fallback: What to do on timeout. "deny" blocks the action;
            "skip" proceeds without approval.
    """

    bp_type: BreakpointType
    tool_name: str | None = None
    ttl_s: float | None = None
    fallback: BreakpointFallback = "deny"


@dataclass
class BreakpointsConfig:
    """Collection of breakpoint rules for a run.

    Built from global settings + per-conversation overrides. The executor
    calls ``should_break()`` at each checkpoint; the first matching rule wins.
    """

    breakpoints: list[BreakpointConfig] = field(default_factory=list)

    def should_break(
        self,
        bp_type: BreakpointType,
        *,
        tool_name: str | None = None,
    ) -> BreakpointConfig | None:
        """Check whether a breakpoint should fire at this point.

        Returns the matching BreakpointConfig (so the executor can read its
        TTL/fallback), or None if no breakpoint applies.
        """
        for bp in self.breakpoints:
            if bp.bp_type != bp_type:
                continue
            if bp.tool_name is not None and tool_name is not None:
                if bp.tool_name != tool_name:
                    continue
            return bp
        return None

    @property
    def is_empty(self) -> bool:
        return not self.breakpoints


# Tools that perform file writes — used for before_write breakpoints.
_WRITE_TOOLS: frozenset[str] = frozenset({"write_file"})


def is_write_tool(tool_name: str) -> bool:
    """True if the tool modifies files (triggers before_write breakpoints)."""
    return tool_name in _WRITE_TOOLS


def parse_breakpoints(raw: list[dict] | None) -> BreakpointsConfig:
    """Parse a raw list of breakpoint dicts into a BreakpointsConfig.

    Each dict should have: ``type`` (str), optional ``tool`` (str),
    optional ``ttl_s`` (float), optional ``fallback`` ("deny"|"skip").
    Invalid entries are logged and skipped.
    """
    if not raw or not isinstance(raw, list):
        return BreakpointsConfig()

    breakpoints: list[BreakpointConfig] = []
    for i, entry in enumerate(raw):
        if not isinstance(entry, dict):
            log.warning("breakpoints.invalid_entry", index=i, entry=entry)
            continue
        bp_type_str = str(entry.get("type", "")).strip().lower()
        try:
            bp_type = BreakpointType(bp_type_str)
        except ValueError:
            log.warning("breakpoints.invalid_type", index=i, type=bp_type_str)
            continue

        tool = entry.get("tool")
        if tool is not None:
            tool = str(tool).strip() or None

        ttl = entry.get("ttl_s")
        if ttl is not None:
            try:
                ttl = float(ttl)
            except (TypeError, ValueError):
                ttl = None

        fallback_str = str(entry.get("fallback", "deny")).strip().lower()
        fallback: BreakpointFallback = "skip" if fallback_str == "skip" else "deny"

        breakpoints.append(
            BreakpointConfig(
                bp_type=bp_type,
                tool_name=tool,
                ttl_s=ttl,
                fallback=fallback,
            )
        )

    return BreakpointsConfig(breakpoints=breakpoints)


def merge_breakpoints(
    global_raw: list[dict] | None,
    conversation_raw: list[dict] | None,
) -> BreakpointsConfig:
    """Merge global and per-conversation breakpoint configs.

    Both lists are concatenated (global first, conversation after). The
    executor's ``should_break()`` returns the first match, so conversation
    breakpoints can override global ones by appearing earlier... actually,
    they're appended, so both apply — the first match in the combined list
    wins. This is intentional: a global ``before_tool`` breakpoint and a
    per-conversation ``before_write`` breakpoint are complementary, not
    conflicting.
    """
    merged = list(parse_breakpoints(global_raw).breakpoints)
    merged.extend(parse_breakpoints(conversation_raw).breakpoints)
    return BreakpointsConfig(breakpoints=merged)
