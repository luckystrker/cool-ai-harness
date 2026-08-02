"""MCP tool bridge: registers MCP-discovered tools in the global ToolRegistry (Фаза 2 §4).

Each tool exposed by a connected MCP server is wrapped as a standard ``Tool``
instance (from ``app/tools/base.py``) with:
- A qualified name ``mcp_{server}_{tool}`` to avoid collisions.
- A dynamically generated ``ToolArgs`` model built from the MCP tool's JSON Schema.
- A func that routes the call through the MCP registry to the server.
- Capabilities inherited from the server config (or empty for global policy).

This module is called after MCP servers connect (on app startup or manual
reconnect) and on disconnect (to unregister stale tools).
"""

from __future__ import annotations

from typing import Any, Optional

from pydantic import create_model

from app.core.logging import get_logger
from app.mcp.models import MCPToolInfo
from app.mcp.registry import get_mcp_registry
from app.security.capabilities import Capability
from app.tools.base import ToolArgs, ToolResult, get_registry, register_tool

log = get_logger(__name__)

# Track which MCP tools we've registered so we can clean them up.
_registered_mcp_tools: set[str] = set()

# JSON Schema type -> Python type mapping for dynamic model fields.
_TYPE_MAP: dict[str, type] = {
    "string": str,
    "integer": int,
    "number": float,
    "boolean": bool,
    "array": list,
    "object": dict,
}


def _build_args_model(tool_info: MCPToolInfo) -> type[ToolArgs]:
    """Dynamically create a ToolArgs subclass from an MCP tool's input schema.

    The MCP ``inputSchema`` is a JSON Schema object with ``properties`` and
    optional ``required`` array.
    """
    schema = tool_info.input_schema
    properties: dict[str, Any] = schema.get("properties", {})
    required_fields: set[str] = set(schema.get("required", []))

    field_definitions: dict[str, Any] = {}
    for prop_name, prop_schema in properties.items():
        if not isinstance(prop_schema, dict):
            continue
        json_type = prop_schema.get("type", "string")
        python_type = _TYPE_MAP.get(json_type, Any)
        default = prop_schema.get("default", ...)

        if prop_name in required_fields and default is ...:
            # Required field with no default.
            field_definitions[prop_name] = (python_type, ...)
        else:
            # Optional field.
            if default is ...:
                default = None
                python_type = Optional[python_type]  # type: ignore[assignment]  # noqa: UP045
            field_definitions[prop_name] = (python_type, default)

    # Create the model dynamically.
    model_name = f"MCP_{tool_info.server_name}_{tool_info.name}_Args"
    model = create_model(
        model_name,
        __base__=ToolArgs,
        **field_definitions,
    )
    return model


def _make_tool_func(tool_info: MCPToolInfo):
    """Create an async func that routes calls through the MCP registry."""
    qualified_name = tool_info.qualified_name

    async def _mcp_tool_call(**kwargs: Any) -> ToolResult:
        registry = get_mcp_registry()
        try:
            output = await registry.call_tool(qualified_name, kwargs)
            return ToolResult.ok(output)
        except Exception as exc:
            return ToolResult.err(f"MCP tool error: {exc}")

    _mcp_tool_call.__name__ = f"mcp_{tool_info.server_name}_{tool_info.name}"
    _mcp_tool_call.__qualname__ = _mcp_tool_call.__name__
    return _mcp_tool_call


def _resolve_capabilities(tool_info: MCPToolInfo) -> frozenset[Capability] | None:
    """Resolve capabilities from the server config."""
    registry = get_mcp_registry()
    state = registry.get_server(tool_info.server_name)
    if state is None or not state.config.capabilities:
        return None
    caps: set[Capability] = set()
    for cap_name in state.config.capabilities:
        try:
            caps.add(Capability(cap_name))
        except ValueError:
            log.warning("mcp.unknown_capability", server=tool_info.server_name, cap=cap_name)
    return frozenset(caps) if caps else None


def register_mcp_tools(tools: list[MCPToolInfo] | None = None) -> int:
    """Register MCP tools in the global ToolRegistry.

    If ``tools`` is None, discovers them from all connected servers.
    Returns the number of tools registered.
    """
    registry = get_mcp_registry()
    if tools is None:
        tools = registry.all_tools()

    count = 0
    for tool_info in tools:
        qualified_name = tool_info.qualified_name
        args_model = _build_args_model(tool_info)
        func = _make_tool_func(tool_info)
        capabilities = _resolve_capabilities(tool_info)

        # Deny-by-default: MCP tools without explicitly declared capabilities
        # are marked dangerous so they go through the approval gate.
        is_dangerous = capabilities is None

        register_tool(
            name=qualified_name,
            description=f"[MCP:{tool_info.server_name}] {tool_info.description}",
            args_model=args_model,
            func=func,
            dangerous=is_dangerous,
            capabilities=capabilities,
        )
        _registered_mcp_tools.add(qualified_name)
        count += 1

    if count:
        log.info("mcp.tools_registered", count=count)
    return count


def unregister_mcp_tools(server_name: str | None = None) -> int:
    """Remove MCP tools from the global registry.

    If ``server_name`` is given, only removes tools from that server.
    Otherwise removes all MCP tools.
    """
    tool_registry = get_registry()
    to_remove: list[str] = []

    for name in _registered_mcp_tools:
        if server_name is None or name.startswith(f"mcp_{server_name}_"):
            to_remove.append(name)

    for name in to_remove:
        tool_registry.pop(name, None)
        _registered_mcp_tools.discard(name)

    if to_remove:
        log.info("mcp.tools_unregistered", count=len(to_remove), server=server_name or "all")
    return len(to_remove)


def refresh_mcp_tools() -> int:
    """Full refresh: unregister all MCP tools, then re-register from connected servers."""
    unregister_mcp_tools()
    return register_mcp_tools()
