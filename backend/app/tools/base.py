"""Tool primitives: Tool class, ToolResult, and the registry.

A "tool" is an async callable plus a Pydantic args model. The model's JSON
Schema becomes the OpenAI-style function parameters spec sent to the LLM.
Tools live in a module-level registry that the builtin tools package populates
on import; tests can construct an isolated registry via ``register_tool(...,
registry=...)``.
"""

from __future__ import annotations

import inspect
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from pydantic import BaseModel

if TYPE_CHECKING:
    from app.security.capabilities import Capability


@dataclass
class ToolResult:
    """Standardized return value of a tool execution.

    ``output`` is the string the model sees as the tool response. Structured
    results should be JSON-encoded by the tool itself; the agent loop does no
    further transformation.
    """

    output: str
    error: str | None = None
    is_error: bool = False
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def ok(cls, output: Any, **metadata: Any) -> ToolResult:
        """Build a success result. ``output`` is coerced to ``str``."""
        if not isinstance(output, str):
            import json

            output = json.dumps(output, ensure_ascii=False, default=str)
        return cls(output=output, metadata=metadata or None)  # type: ignore[arg-type]

    @classmethod
    def err(cls, error: str, **metadata: Any) -> ToolResult:
        """Build an error result."""
        return cls(output=error, error=error, is_error=True, metadata=metadata or None)  # type: ignore[arg-type]


class ToolArgs(BaseModel):
    """Base class for typed tool arguments. Subclass per tool."""

    model_config = {"arbitrary_types_allowed": True}


ToolFunc = Callable[..., Awaitable[ToolResult]]


@dataclass
class Tool:
    """A registered tool: callable + schema + metadata."""

    name: str
    description: str
    func: ToolFunc
    args_model: type[ToolArgs]
    dangerous: bool = False  # e.g. code execution; surfaced in UI/scheduler
    # Capabilities this tool requires (read, write, execute, network, git,
    # send_external). Used by the capability security layer to gate access.
    # None/empty = no capability gating (per-tool permissions still apply).
    capabilities: frozenset[Capability] | None = None

    def parameters_schema(self) -> dict[str, Any]:
        """JSON Schema for this tool's arguments (OpenAI function-calling shape)."""
        schema = self.args_model.model_json_schema()
        schema.pop("title", None)
        # Inline $defs: OpenAI accepts $defs, but flatter schemas are friendlier
        # for some providers. We leave $defs in place for correctness.
        return schema

    async def run(self, arguments: dict[str, Any] | None = None) -> ToolResult:
        """Validate arguments against the model and invoke the tool."""
        arguments = arguments or {}
        try:
            parsed = self.args_model.model_validate(arguments)
        except Exception as exc:
            return ToolResult.err(f"Invalid arguments: {exc}")
        try:
            return await self.func(**parsed.model_dump())
        except Exception as exc:
            return ToolResult.err(f"Tool {self.name!r} raised: {exc}")


# Module-level registry. Builtin tools populate it on import (see
# app/tools/__init__.py). Tests use clear_registry() or pass registry=.
_REGISTRY: dict[str, Tool] = {}


def register_tool(
    *,
    name: str,
    description: str,
    args_model: type[ToolArgs],
    func: ToolFunc,
    dangerous: bool = False,
    capabilities: frozenset[Capability] | None = None,
    registry: dict[str, Tool] | None = None,
) -> Tool:
    """Register a tool. Returns the Tool instance for chaining."""
    if not inspect.iscoroutinefunction(func):
        raise TypeError(f"Tool {name!r}: func must be async")
    _reg = registry if registry is not None else _REGISTRY
    instance = Tool(
        name=name,
        description=description,
        func=func,
        args_model=args_model,
        dangerous=dangerous,
        capabilities=capabilities,
    )
    _reg[name] = instance
    return instance


def get_registry() -> dict[str, Tool]:
    """Return the global registry (mutated by builtin tools on import)."""
    return _REGISTRY


def get_tool(name: str) -> Tool | None:
    """Look up a tool by name in the global registry."""
    return _REGISTRY.get(name)


def clear_registry() -> None:
    """Drop all registered tools. Intended for tests."""
    _REGISTRY.clear()
