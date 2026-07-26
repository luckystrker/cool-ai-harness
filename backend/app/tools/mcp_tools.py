"""MCP management tools (Фаза 2 §4).

Provides tools that let the agent manage MCP servers from within a conversation:
- ``mcp_list_servers``: show configured servers and their status/tools.
- ``mcp_add_server``: add a new MCP server configuration.
- ``mcp_remove_server``: remove a server and disconnect it.
- ``mcp_connect_server``: connect to a server and discover its tools.
- ``mcp_disconnect_server``: disconnect a running server.
- ``mcp_search_store``: search the official MCP Registry for installable servers.
- ``mcp_install_server``: install a server from the registry (add + connect).

These tools are registered alongside the builtin tools so the agent can
autonomously manage its MCP server fleet during a conversation.
"""

from __future__ import annotations

from pydantic import Field

from app.core.logging import get_logger
from app.mcp.models import MCPServerConfig, MCPServerStatus, MCPTransport
from app.tools.base import ToolArgs, ToolResult, register_tool

log = get_logger(__name__)


# --- mcp_list_servers ---


class MCPListServersArgs(ToolArgs):
    """Arguments for the mcp_list_servers tool."""

    pass


async def _mcp_list_servers() -> ToolResult:
    """List all configured MCP servers with their status and tools."""
    from app.mcp.registry import get_mcp_registry

    registry = get_mcp_registry()
    servers = registry.list_servers()

    if not servers:
        return ToolResult.ok(
            "No MCP servers configured. Use mcp_add_server to add one, "
            "or mcp_search_store to browse the MCP Registry."
        )

    lines = [f"MCP Servers ({len(servers)}):"]
    for state in servers:
        status_icon = {
            MCPServerStatus.CONNECTED: "●",
            MCPServerStatus.CONNECTING: "◐",
            MCPServerStatus.ERROR: "✗",
            MCPServerStatus.DISCONNECTED: "○",
        }.get(state.status, "?")
        enabled_str = "" if state.config.enabled else " [disabled]"
        lines.append(
            f"- {status_icon} **{state.name}** ({state.config.transport.value}) "
            f"[{state.status.value}]{enabled_str}"
        )
        if state.config.description:
            lines.append(f"  {state.config.description}")
        if state.tools:
            tool_names = ", ".join(t.name for t in state.tools[:10])
            suffix = f" (+{len(state.tools) - 10} more)" if len(state.tools) > 10 else ""
            lines.append(f"  Tools: {tool_names}{suffix}")
        if state.error:
            lines.append(f"  Error: {state.error}")

    return ToolResult.ok("\n".join(lines))


# --- mcp_add_server ---


class MCPAddServerArgs(ToolArgs):
    """Arguments for the mcp_add_server tool."""

    name: str = Field(description="Unique server name (lowercase, hyphens/underscores)")
    transport: str = Field(default="stdio", description="Transport: 'stdio' or 'http'")
    command: str = Field(default="", description="Executable command for stdio (e.g. 'npx', 'python')")
    args: list[str] = Field(default_factory=list, description="Command arguments for stdio")
    url: str = Field(default="", description="Server URL for HTTP transport")
    description: str = Field(default="", description="Human-readable description")
    auto_connect: bool = Field(default=True, description="Connect immediately after adding")


async def _mcp_add_server(
    name: str,
    transport: str = "stdio",
    command: str = "",
    args: list[str] | None = None,
    url: str = "",
    description: str = "",
    auto_connect: bool = True,
) -> ToolResult:
    """Add a new MCP server configuration and optionally connect."""
    from app.mcp.registry import get_mcp_registry
    from app.mcp.tool_bridge import refresh_mcp_tools

    args = args or []
    registry = get_mcp_registry()

    if registry.get_server(name) is not None:
        return ToolResult.err(f"Server '{name}' already exists. Remove it first or use a different name.")

    try:
        transport_enum = MCPTransport(transport)
    except ValueError:
        return ToolResult.err(f"Invalid transport '{transport}'. Must be 'stdio' or 'http'.")

    if transport_enum == MCPTransport.STDIO and not command:
        return ToolResult.err("A 'command' is required for stdio transport (e.g. 'npx', 'python').")
    if transport_enum == MCPTransport.HTTP and not url:
        return ToolResult.err("A 'url' is required for HTTP transport.")

    config = MCPServerConfig(
        name=name,
        transport=transport_enum,
        command=command,
        args=args,
        url=url,
        description=description,
        enabled=True,
    )
    registry.add_server(config)
    _persist()
    log.info("mcp.tool.server_added", name=name, transport=transport)

    if auto_connect:
        state = await registry.connect_server(name)
        if state.status == MCPServerStatus.CONNECTED:
            refresh_mcp_tools()
            tool_names = ", ".join(t.name for t in state.tools[:8])
            return ToolResult.ok(
                f"Server '{name}' added and connected. "
                f"Discovered {state.tool_count} tools: {tool_names}"
            )
        return ToolResult.ok(
            f"Server '{name}' added but connection failed: {state.error}. "
            "You can retry with mcp_connect_server."
        )

    return ToolResult.ok(f"Server '{name}' added (not connected). Use mcp_connect_server to connect.")


# --- mcp_remove_server ---


class MCPRemoveServerArgs(ToolArgs):
    """Arguments for the mcp_remove_server tool."""

    name: str = Field(description="Name of the server to remove")


async def _mcp_remove_server(name: str) -> ToolResult:
    """Remove an MCP server configuration and disconnect it."""
    from app.mcp.registry import get_mcp_registry
    from app.mcp.tool_bridge import unregister_mcp_tools

    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        available = [s.name for s in registry.list_servers()]
        hint = f" Available servers: {', '.join(available)}" if available else ""
        return ToolResult.err(f"Server '{name}' not found.{hint}")

    unregister_mcp_tools(server_name=name)
    await registry.disconnect_server(name)
    registry.remove_server(name)
    _persist()
    log.info("mcp.tool.server_removed", name=name)
    return ToolResult.ok(f"Server '{name}' removed and disconnected.")


# --- mcp_connect_server ---


class MCPConnectServerArgs(ToolArgs):
    """Arguments for the mcp_connect_server tool."""

    name: str = Field(description="Name of the server to connect")


async def _mcp_connect_server(name: str) -> ToolResult:
    """Connect to an MCP server and discover its tools."""
    from app.mcp.registry import get_mcp_registry
    from app.mcp.tool_bridge import refresh_mcp_tools

    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        return ToolResult.err(f"Server '{name}' not found. Use mcp_add_server first.")

    state = await registry.connect_server(name)
    if state.status == MCPServerStatus.CONNECTED:
        refresh_mcp_tools()
        tool_names = ", ".join(t.name for t in state.tools[:10])
        return ToolResult.ok(
            f"Connected to '{name}'. Discovered {state.tool_count} tools: {tool_names}"
        )
    return ToolResult.err(f"Failed to connect to '{name}': {state.error}")


# --- mcp_disconnect_server ---


class MCPDisconnectServerArgs(ToolArgs):
    """Arguments for the mcp_disconnect_server tool."""

    name: str = Field(description="Name of the server to disconnect")


async def _mcp_disconnect_server(name: str) -> ToolResult:
    """Disconnect an MCP server and unregister its tools."""
    from app.mcp.registry import get_mcp_registry
    from app.mcp.tool_bridge import unregister_mcp_tools

    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        return ToolResult.err(f"Server '{name}' not found.")

    unregister_mcp_tools(server_name=name)
    await registry.disconnect_server(name)
    return ToolResult.ok(f"Server '{name}' disconnected. Its tools are no longer available.")


# --- mcp_search_store ---


class MCPSearchStoreArgs(ToolArgs):
    """Arguments for the mcp_search_store tool."""

    query: str = Field(description="Search query (e.g. 'filesystem', 'github', 'sql')")
    limit: int = Field(default=5, description="Max results to return (1-20)")


async def _mcp_search_store(query: str, limit: int = 5) -> ToolResult:
    """Search the official MCP Registry for available servers."""
    from app.mcp.marketplace import search_registry

    try:
        results = await search_registry(query, limit=min(limit, 20))
    except Exception as exc:
        return ToolResult.err(f"MCP Registry search failed: {exc}")

    if not results:
        return ToolResult.ok(f"No MCP servers found for '{query}' in the registry.")

    lines = [f"MCP Registry results for '{query}' ({len(results)} found):"]
    for item in results:
        lines.append(f"- **{item['name']}** (v{item.get('version', '?')})")
        if item.get("description"):
            lines.append(f"  {item['description']}")
        if item.get("install_command"):
            lines.append(f"  Install: `{item['install_command']}`")
        if item.get("transport"):
            lines.append(f"  Transport: {item['transport']}")

    lines.append("\nUse mcp_install_server to install one of these.")
    return ToolResult.ok("\n".join(lines))


# --- mcp_install_server ---


class MCPInstallServerArgs(ToolArgs):
    """Arguments for the mcp_install_server tool."""

    registry_name: str = Field(
        description="Full registry name (e.g. 'io.github.modelcontextprotocol/servers-filesystem')"
    )
    server_name: str = Field(
        default="",
        description="Local name for the server (default: derived from registry name)",
    )


async def _mcp_install_server(registry_name: str, server_name: str = "") -> ToolResult:
    """Install an MCP server from the official registry (add config + connect)."""
    from app.mcp.marketplace import get_server_details, registry_entry_to_config
    from app.mcp.registry import get_mcp_registry
    from app.mcp.tool_bridge import refresh_mcp_tools

    try:
        entry = await get_server_details(registry_name)
    except Exception as exc:
        return ToolResult.err(f"Failed to fetch '{registry_name}' from registry: {exc}")

    if entry is None:
        return ToolResult.err(
            f"Server '{registry_name}' not found in the MCP Registry. "
            "Use mcp_search_store to find available servers."
        )

    config = registry_entry_to_config(entry, local_name=server_name or "")
    if config is None:
        return ToolResult.err(
            f"Server '{registry_name}' has no installable packages with stdio/HTTP transport."
        )

    registry = get_mcp_registry()
    if registry.get_server(config.name) is not None:
        return ToolResult.err(
            f"Server '{config.name}' already exists locally. "
            "Remove it first or choose a different name via server_name."
        )

    registry.add_server(config)
    _persist()

    state = await registry.connect_server(config.name)
    if state.status == MCPServerStatus.CONNECTED:
        refresh_mcp_tools()
        tool_names = ", ".join(t.name for t in state.tools[:8])
        return ToolResult.ok(
            f"Installed and connected '{config.name}' from MCP Registry.\n"
            f"Description: {config.description}\n"
            f"Discovered {state.tool_count} tools: {tool_names}"
        )

    return ToolResult.ok(
        f"Installed '{config.name}' but connection failed: {state.error}.\n"
        "The server is configured — retry with mcp_connect_server when ready."
    )


# --- Helpers ---


def _persist() -> None:
    """Save current configs to config.yaml."""
    from app.mcp.config import save_mcp_configs
    from app.mcp.registry import get_mcp_registry

    registry = get_mcp_registry()
    configs = [s.config for s in registry.list_servers()]
    save_mcp_configs(configs)


# --- Registration ---


def register_mcp_management_tools() -> None:
    """Register MCP management tools. Idempotent."""
    register_tool(
        name="mcp_list_servers",
        description=(
            "List all configured MCP servers with their connection status and "
            "discovered tools. MCP servers extend your capabilities with external tools."
        ),
        args_model=MCPListServersArgs,
        func=_mcp_list_servers,
    )
    register_tool(
        name="mcp_add_server",
        description=(
            "Add a new MCP server configuration. Supports stdio (subprocess command) "
            "and HTTP transports. Optionally auto-connects to discover tools immediately."
        ),
        args_model=MCPAddServerArgs,
        func=_mcp_add_server,
    )
    register_tool(
        name="mcp_remove_server",
        description="Remove an MCP server configuration and disconnect it.",
        args_model=MCPRemoveServerArgs,
        func=_mcp_remove_server,
    )
    register_tool(
        name="mcp_connect_server",
        description=(
            "Connect to a configured MCP server and discover its tools. "
            "Tools become available in the agent's toolset after connection."
        ),
        args_model=MCPConnectServerArgs,
        func=_mcp_connect_server,
    )
    register_tool(
        name="mcp_disconnect_server",
        description="Disconnect an MCP server. Its tools will no longer be available.",
        args_model=MCPDisconnectServerArgs,
        func=_mcp_disconnect_server,
    )
    register_tool(
        name="mcp_search_store",
        description=(
            "Search the official MCP Registry (registry.modelcontextprotocol.io) for "
            "installable MCP servers. Returns server names, descriptions, and install info. "
            "Use mcp_install_server to install a result."
        ),
        args_model=MCPSearchStoreArgs,
        func=_mcp_search_store,
    )
    register_tool(
        name="mcp_install_server",
        description=(
            "Install an MCP server from the official MCP Registry by its registry name "
            "(e.g. 'io.github.modelcontextprotocol/servers-filesystem'). "
            "Automatically configures, connects, and registers the server's tools."
        ),
        args_model=MCPInstallServerArgs,
        func=_mcp_install_server,
    )
