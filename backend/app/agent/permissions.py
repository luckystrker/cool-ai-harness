"""Tool permission resolution.

Permissions decide, per tool call, whether the agent may run it outright
(``allow``), must ask the user first (``ask``), or may never run it (``deny``).

Configuration shape (stored on Conversation.permissions and on
Settings.default_tool_permissions)::

    {"*": "ask", "read_file": "allow", "python_execute": "deny"}

Resolution order for a tool ``T``:
    1. per-conversation explicit entry for ``T``
    2. per-conversation ``"*"`` wildcard
    3. global explicit entry for ``T``
    4. global ``"*"`` wildcard
    5. fallback: ``"allow"`` for safe tools (``dangerous=False``),
       ``"ask"`` for dangerous ones

``"ask"`` means the executor emits a ``tool_approval_request`` event and waits
for the client to approve/deny via the approval endpoint.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

from app.core.logging import get_logger

log = get_logger(__name__)

# A per-tool decision.
Decision = Literal["allow", "ask", "deny"]

VALID_DECISIONS: frozenset[str] = frozenset({"allow", "ask", "deny"})


@dataclass
class PermissionsConfig:
    """Effective permission map for a run (global + conversation merged).

    ``tools`` maps tool name -> Decision. The ``"*"`` key is the catch-all.
    Lookups via :meth:`resolve`; never index directly so the wildcard and the
    fallback are honored.
    """

    tools: dict[str, str] = field(default_factory=dict)

    def resolve(self, tool_name: str, *, dangerous: bool = False) -> Decision:
        """Resolve the effective decision for ``tool_name``.

        Precedence: explicit name > ``"*"`` > dangerous-aware fallback. The
        fallback is ``"allow"`` for safe tools (``dangerous=False``, e.g.
        ``web_search`` / ``read_file``) so read-only tools run straight through
        out of the box, and ``"ask"`` for dangerous ones (``python_execute``)
        so side-effecting tools still require confirmation. Explicit entries
        and the wildcard always take precedence over the fallback.
        """
        if tool_name in self.tools:
            decision = self.tools[tool_name]
        elif "*" in self.tools:
            decision = self.tools["*"]
        else:
            # No policy applies. Safe tools run by default; dangerous ones
            # (code execution, destructive ops) still prompt.
            decision = "ask" if dangerous else "allow"
        return _coerce_decision(decision, tool_name)  # type: ignore[return-value]

    def to_dict(self) -> dict[str, str]:
        return dict(self.tools)


def _coerce_decision(value: str, tool_name: str) -> Decision:
    """Normalize a stored value to a Decision, logging + falling back on bad input."""
    v = str(value).strip().lower()
    if v in VALID_DECISIONS:
        return v  # type: ignore[return-value]
    log.warning("permissions.invalid_decision", tool=tool_name, value=value)
    return "ask"


def normalize(raw: dict | None) -> dict[str, str]:
    """Coerce a raw permission map into a clean {tool: decision} dict.

    Drops entries with invalid decisions (logging them) rather than raising,
    so a bad conversation.permissions value can't break the run.
    """
    if not raw or not isinstance(raw, dict):  # type: ignore[unreachable]
        return {}
    clean: dict[str, str] = {}
    for key, value in raw.items():
        name = str(key)
        v = str(value).strip().lower()
        if v in VALID_DECISIONS:
            clean[name] = v
        else:
            log.warning("permissions.invalid_entry", tool=name, value=value)
    return clean


def validate(raw: dict | None) -> list[str]:
    """Return a list of human-readable errors for invalid entries (empty = OK).

    Used by the API layer to 400 on a malformed permissions payload instead of
    silently dropping entries.
    """
    if raw is None:
        return []
    if not isinstance(raw, dict):
        return ["permissions must be a JSON object mapping tool name to allow|ask|deny"]
    errors: list[str] = []
    for key, value in raw.items():
        v = str(value).strip().lower()
        if v not in VALID_DECISIONS:
            errors.append(
                f"permissions[{key!r}] = {value!r} is not one of allow|ask|deny"
            )
    return errors


def merge(global_map: dict[str, str] | None, conversation_map: dict[str, str] | None) -> PermissionsConfig:
    """Merge global and per-conversation permission maps.

    Per-conversation entries win on conflict (same tool name). Both maps may
    contribute a ``"*"`` wildcard; the conversation wildcard wins too. The
    result is the effective map used for the run.
    """
    merged: dict[str, str] = {}
    merged.update(normalize(global_map))
    # Conversation overrides take precedence.
    merged.update(normalize(conversation_map))
    return PermissionsConfig(tools=merged)
