"""Capability model: coarse-grained permission categories.

Each tool declares one or more capabilities it requires. A capability policy
maps each capability to ``allow`` | ``ask`` | ``deny``. The executor resolves
both the capability layer and the per-tool permission layer:

    effective = stricter(capability_decision, tool_decision)

where ``stricter`` picks the more restrictive of the two (deny > ask > allow).

Capabilities:
    read          — reading files, listing directories
    write         — writing/modifying files
    execute       — running code / subprocesses
    network       — HTTP requests, web search/fetch
    git           — git operations (Фаза 4)
    send_external — sending data to external services (Telegram, webhooks)

Scopes (workspace paths, network domains) are enforced by the tools
themselves (file_tools confines to workspace; web_tools checks the SSRF
allowlist). The capability layer is about *whether* the category is allowed
at all, not *what specific resources* within it.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Literal

from app.core.logging import get_logger

log = get_logger(__name__)

Decision = Literal["allow", "ask", "deny"]

_VALID_DECISIONS: frozenset[str] = frozenset({"allow", "ask", "deny"})

# Precedence when merging two decisions: the more restrictive wins.
_DECISION_RANK: dict[str, int] = {"allow": 0, "ask": 1, "deny": 2}


class Capability(str, Enum):
    """Coarse-grained capability categories."""

    READ = "read"
    WRITE = "write"
    EXECUTE = "execute"
    NETWORK = "network"
    GIT = "git"
    SEND_EXTERNAL = "send_external"


# Default capability mapping for builtin tools. Tools can also declare their
# capabilities directly on the Tool dataclass (see app/tools/base.py); this
# map is the fallback when a tool doesn't declare them.
_DEFAULT_TOOL_CAPS: dict[str, frozenset[Capability]] = {
    "read_file": frozenset({Capability.READ}),
    "write_file": frozenset({Capability.WRITE}),
    "list_files": frozenset({Capability.READ}),
    "python_execute": frozenset({Capability.EXECUTE}),
    "web_search": frozenset({Capability.NETWORK}),
    "web_fetch": frozenset({Capability.NETWORK}),
}


def tool_capabilities(tool_name: str) -> frozenset[Capability]:
    """Return the capabilities a tool requires.

    Looks up the tool in the registry first (if it declares capabilities),
    then falls back to the default mapping. Unknown tools get an empty set
    (no capability gating), so the per-tool permission system still applies.
    """
    try:
        from app.tools import get_tool

        tool = get_tool(tool_name)
        if tool is not None and tool.capabilities:
            return frozenset(tool.capabilities)
    except Exception:
        pass
    return _DEFAULT_TOOL_CAPS.get(tool_name, frozenset())


@dataclass
class CapabilityPolicy:
    """Maps capability names to allow|ask|deny decisions.

    Built from Settings.capability_policy (global) optionally merged with a
    per-conversation override. Resolution: explicit capability entry, then
    the ``"*"`` wildcard, then fallback ``allow`` (the capability layer is
    opt-in — if no policy is configured, it doesn't restrict anything).
    """

    caps: dict[str, str] = field(default_factory=dict)

    def resolve(self, capability: Capability) -> Decision:
        """Resolve a single capability to a decision."""
        name = capability.value
        if name in self.caps:
            decision = self.caps[name]
        elif "*" in self.caps:
            decision = self.caps["*"]
        else:
            return "allow"
        return _coerce_decision(decision, name)

    def resolve_tool(self, tool_name: str) -> Decision:
        """Resolve the effective decision for a tool based on its capabilities.

        Returns the *most restrictive* decision across all capabilities the
        tool requires. If the tool has no declared capabilities, returns
        ``allow`` (capability gating is opt-in).
        """
        caps = tool_capabilities(tool_name)
        if not caps:
            return "allow"
        result: Decision = "allow"
        for cap in caps:
            d = self.resolve(cap)
            if _DECISION_RANK[d] > _DECISION_RANK[result]:
                result = d
            if result == "deny":
                break
        return result

    def to_dict(self) -> dict[str, str]:
        return dict(self.caps)


def _coerce_decision(value: str, name: str) -> Decision:
    v = str(value).strip().lower()
    if v in _VALID_DECISIONS:
        return v  # type: ignore[return-value]
    log.warning("capabilities.invalid_decision", capability=name, value=value)
    return "ask"


def normalize_policy(raw: dict | None) -> dict[str, str]:
    """Coerce a raw capability policy into a clean {cap: decision} dict."""
    if not raw or not isinstance(raw, dict):  # type: ignore[unreachable]
        return {}
    clean: dict[str, str] = {}
    for key, value in raw.items():
        name = str(key)
        v = str(value).strip().lower()
        if v in _VALID_DECISIONS:
            clean[name] = v
        else:
            log.warning("capabilities.invalid_entry", capability=name, value=value)
    return clean


def validate_policy(raw: dict | None) -> list[str]:
    """Return human-readable errors for invalid entries (empty = OK)."""
    if raw is None:
        return []
    if not isinstance(raw, dict):
        return ["capability_policy must be a JSON object mapping capability to allow|ask|deny"]
    errors: list[str] = []
    for key, value in raw.items():
        v = str(value).strip().lower()
        if v not in _VALID_DECISIONS:
            errors.append(
                f"capability_policy[{key!r}] = {value!r} is not one of allow|ask|deny"
            )
    return errors


def merge_policy(
    global_map: dict[str, str] | None,
    conversation_map: dict[str, str] | None,
) -> CapabilityPolicy:
    """Merge global and per-conversation capability policies.

    Per-conversation entries win on conflict. Both maps may contribute a
    ``"*"`` wildcard; the conversation wildcard wins too.
    """
    merged: dict[str, str] = {}
    merged.update(normalize_policy(global_map))
    merged.update(normalize_policy(conversation_map))
    return CapabilityPolicy(caps=merged)


def stricter(a: Decision, b: Decision) -> Decision:
    """Return the more restrictive of two decisions (deny > ask > allow)."""
    return a if _DECISION_RANK[a] >= _DECISION_RANK[b] else b
