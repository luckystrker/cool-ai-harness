"""MCP (Model Context Protocol) client subsystem (Фаза 2 §4).

Provides:
- ``MCPClient``: async client for stdio/HTTP MCP servers.
- ``MCPRegistry``: server lifecycle management and tool discovery.
- ``load_mcp_configs`` / ``save_mcp_configs``: config.yaml persistence.
- ``register_mcp_tools`` / ``unregister_mcp_tools``: ToolRegistry bridge.
"""

from app.mcp.client import MCPClient, MCPClientError
from app.mcp.config import load_mcp_configs, save_mcp_configs
from app.mcp.models import (
    MCPServerConfig,
    MCPServerState,
    MCPServerStatus,
    MCPToolInfo,
    MCPTransport,
)
from app.mcp.registry import get_mcp_registry, reset_mcp_registry
from app.mcp.tool_bridge import (
    refresh_mcp_tools,
    register_mcp_tools,
    unregister_mcp_tools,
)

__all__ = [
    "MCPClient",
    "MCPClientError",
    "MCPServerConfig",
    "MCPServerState",
    "MCPServerStatus",
    "MCPToolInfo",
    "MCPTransport",
    "get_mcp_registry",
    "load_mcp_configs",
    "refresh_mcp_tools",
    "register_mcp_tools",
    "reset_mcp_registry",
    "save_mcp_configs",
    "unregister_mcp_tools",
]