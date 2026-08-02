"""MCP server registry: lifecycle management and tool discovery (Фаза 2 §4).

The registry holds all configured MCP servers, manages their connections,
and exposes discovered tools for registration in the global ToolRegistry.

Mirrors the SkillRegistry pattern: module-level singleton, lazy loading,
force-reload support.
"""

from __future__ import annotations

import asyncio
from typing import Any

from app.core.logging import get_logger
from app.mcp.client import MCPClient, MCPClientError
from app.mcp.models import (
    MCPServerConfig,
    MCPServerState,
    MCPServerStatus,
    MCPToolInfo,
)

log = get_logger(__name__)


class MCPRegistry:
    """Manages MCP server connections and their discovered tools.

    Usage::

        registry = get_mcp_registry()
        registry.add_server(config)
        await registry.connect_all()
        tools = registry.all_tools()
        await registry.shutdown()
    """

    def __init__(self) -> None:
        self._servers: dict[str, MCPServerState] = {}
        self._clients: dict[str, MCPClient] = {}

    # --- Configuration ---

    def add_server(self, config: MCPServerConfig) -> None:
        """Add or update a server configuration. Does not auto-connect.

        If the server was previously connected, disconnect the old client to
        prevent resource leaks (the new config takes effect on next connect).
        """
        existing = self._servers.get(config.name)
        if existing and existing.status == MCPServerStatus.CONNECTED:
            # Disconnect the old client so it doesn't leak; the new config
            # will be used on the next connect_server() call.
            old_client = self._clients.pop(config.name, None)
            if old_client:
                try:
                    loop = asyncio.get_running_loop()
                    task = loop.create_task(old_client.disconnect())
                    task.add_done_callback(
                        lambda t: t.exception() if not t.cancelled() else None
                    )
                except RuntimeError:
                    pass  # No running loop; client will be GC'd.
            log.info("mcp.server_replacing_connected", name=config.name)
        self._servers[config.name] = MCPServerState(config=config)
        log.info("mcp.server_added", name=config.name, transport=config.transport.value)

    def remove_server(self, name: str) -> bool:
        """Remove a server config. Disconnects if active. Returns True if existed."""
        state = self._servers.pop(name, None)
        if state is None:
            return False
        # Schedule disconnect (fire-and-forget if in async context).
        client = self._clients.pop(name, None)
        if client:
            try:
                loop = asyncio.get_running_loop()
                task = loop.create_task(client.disconnect())
                task.add_done_callback(lambda t: t.exception() if not t.cancelled() else None)
            except RuntimeError:
                pass  # No running loop; client will be GC'd.
        log.info("mcp.server_removed", name=name)
        return True

    def get_server(self, name: str) -> MCPServerState | None:
        """Get the state of a configured server."""
        return self._servers.get(name)

    def list_servers(self) -> list[MCPServerState]:
        """All configured servers with their current state."""
        return sorted(self._servers.values(), key=lambda s: s.name)

    def list_enabled(self) -> list[MCPServerState]:
        """Only enabled servers."""
        return [s for s in self._servers.values() if s.config.enabled]

    # --- Connection lifecycle ---

    async def connect_server(self, name: str) -> MCPServerState:
        """Connect to a specific server and discover its tools."""
        state = self._servers.get(name)
        if state is None:
            raise MCPClientError(f"MCP server '{name}' not configured")

        # Disconnect existing client if any.
        await self._disconnect_client(name)

        state.status = MCPServerStatus.CONNECTING
        state.error = None

        client = MCPClient(state.config)
        try:
            server_info = await client.connect()
            state.server_info = server_info
            tools = await client.list_tools()
            state.tools = tools
            state.status = MCPServerStatus.CONNECTED
            self._clients[name] = client
            log.info("mcp.server_connected", name=name, tools=len(tools))
        except (MCPClientError, Exception) as exc:
            state.status = MCPServerStatus.ERROR
            state.error = str(exc)
            state.tools = []
            await client.disconnect()
            log.error("mcp.server_connect_failed", name=name, error=str(exc))

        return state

    async def connect_all(self) -> None:
        """Connect to all enabled servers concurrently."""
        enabled = [s.name for s in self._servers.values() if s.config.enabled]
        if not enabled:
            return
        tasks = [self.connect_server(name) for name in enabled]
        await asyncio.gather(*tasks, return_exceptions=True)
        connected = sum(1 for s in self._servers.values() if s.status == MCPServerStatus.CONNECTED)
        log.info("mcp.connect_all_done", total=len(enabled), connected=connected)

    async def disconnect_server(self, name: str) -> None:
        """Disconnect a specific server."""
        await self._disconnect_client(name)
        state = self._servers.get(name)
        if state:
            state.status = MCPServerStatus.DISCONNECTED
            state.tools = []

    async def shutdown(self) -> None:
        """Disconnect all servers. Called on app shutdown."""
        for name in list(self._clients.keys()):
            await self._disconnect_client(name)
        for state in self._servers.values():
            state.status = MCPServerStatus.DISCONNECTED
            state.tools = []
        log.info("mcp.shutdown_complete")

    async def _disconnect_client(self, name: str) -> None:
        client = self._clients.pop(name, None)
        if client:
            try:
                await client.disconnect()
            except Exception as exc:
                log.warning("mcp.disconnect_error", name=name, error=str(exc))

    # --- Tool access ---

    def all_tools(self) -> list[MCPToolInfo]:
        """All tools from all connected servers."""
        tools: list[MCPToolInfo] = []
        for state in self._servers.values():
            if state.status == MCPServerStatus.CONNECTED:
                tools.extend(state.tools)
        return tools

    def get_tool(self, qualified_name: str) -> MCPToolInfo | None:
        """Look up a tool by its qualified name (mcp_{server}_{tool})."""
        for state in self._servers.values():
            if state.status != MCPServerStatus.CONNECTED:
                continue
            for tool in state.tools:
                if tool.qualified_name == qualified_name:
                    return tool
        return None

    def get_client(self, server_name: str) -> MCPClient | None:
        """Get the active client for a server (for tool calls)."""
        return self._clients.get(server_name)

    async def call_tool(self, qualified_name: str, arguments: dict[str, Any] | None = None) -> str:
        """Route a tool call to the appropriate MCP server.

        ``qualified_name`` is ``mcp_{server}_{tool}``.
        """
        # Parse server name and tool name from qualified name.
        for state in self._servers.values():
            if state.status != MCPServerStatus.CONNECTED:
                continue
            for tool in state.tools:
                if tool.qualified_name == qualified_name:
                    client = self._clients.get(state.name)
                    if client is None:
                        raise MCPClientError(f"No active client for server '{state.name}'")
                    return await client.call_tool(tool.name, arguments)

        raise MCPClientError(f"MCP tool '{qualified_name}' not found in any connected server")

    # --- Health ---

    async def health_check(self, name: str) -> bool:
        """Ping a connected server."""
        client = self._clients.get(name)
        if client is None:
            return False
        return await client.ping()

    # --- Bulk config ---

    def load_configs(self, configs: list[MCPServerConfig]) -> None:
        """Load multiple server configs (e.g. from config.yaml). Replaces existing."""
        # Disconnect all current clients.
        for name in list(self._clients.keys()):
            state = self._servers.get(name)
            if state:
                state.status = MCPServerStatus.DISCONNECTED
        self._servers.clear()
        for config in configs:
            self._servers[config.name] = MCPServerState(config=config)
        log.info("mcp.configs_loaded", count=len(configs))


# --- Module-level singleton ---

_registry: MCPRegistry | None = None


def get_mcp_registry() -> MCPRegistry:
    """Return the global MCPRegistry singleton."""
    global _registry
    if _registry is None:
        _registry = MCPRegistry()
    return _registry


def reset_mcp_registry() -> None:
    """Reset the global registry. Intended for tests."""
    global _registry
    _registry = None
