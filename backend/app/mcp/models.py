"""MCP data models: server configuration, tool descriptors, and status (Фаза 2 §4).

These models represent MCP server connections and the tools they expose.
Configuration is loaded from ``config.yaml`` and/or managed via the API/UI.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any


class MCPTransport(StrEnum):
    """Supported MCP transport protocols."""

    STDIO = "stdio"
    HTTP = "http"


class MCPServerStatus(StrEnum):
    """Lifecycle status of an MCP server connection."""

    DISCONNECTED = "disconnected"
    CONNECTING = "connecting"
    CONNECTED = "connected"
    ERROR = "error"


@dataclass
class MCPToolParam:
    """A single parameter in an MCP tool's input schema."""

    name: str
    type: str = "string"
    description: str = ""
    required: bool = False
    enum: list[str] | None = None


@dataclass
class MCPToolInfo:
    """Descriptor for a tool exposed by an MCP server.

    Mirrors the ``tools/list`` response item from the MCP specification.
    """

    name: str
    description: str = ""
    input_schema: dict[str, Any] = field(default_factory=dict)
    server_name: str = ""

    @property
    def qualified_name(self) -> str:
        """Globally unique tool name: ``mcp_{server}_{tool}``."""
        return f"mcp_{self.server_name}_{self.name}"


@dataclass
class MCPServerConfig:
    """Configuration for a single MCP server connection.

    Supports both stdio (subprocess) and HTTP (Streamable HTTP / SSE) transports.
    Includes plugin manifest fields (version, author, compatibility) for
    lifecycle management (Фаза 2 §2).
    """

    name: str
    transport: MCPTransport = MCPTransport.STDIO
    # --- stdio transport ---
    command: str = ""  # e.g. "npx", "python", "node"
    args: list[str] = field(default_factory=list)  # e.g. ["-y", "@modelcontextprotocol/server-filesystem"]
    env: dict[str, str] = field(default_factory=dict)  # extra env vars for the subprocess
    # --- HTTP transport ---
    url: str = ""  # e.g. "http://localhost:8080/mcp"
    headers: dict[str, str] = field(default_factory=dict)  # auth headers
    # --- common ---
    enabled: bool = True
    description: str = ""
    # Capabilities granted to this server's tools (empty = inherit global policy).
    capabilities: list[str] = field(default_factory=list)
    # Timeout for tool calls in seconds.
    timeout_s: float = 30.0
    # --- Plugin manifest (Фаза 2 §2 lifecycle) ---
    version: str = ""  # semver string, e.g. "1.2.0"
    author: str = ""
    # Minimum harness version required (for forward-compatibility checks).
    compatibility: str = ""

    def to_dict(self) -> dict[str, Any]:
        """Serialize to a JSON-compatible dict (for config.yaml / API responses)."""
        d: dict[str, Any] = {
            "name": self.name,
            "transport": self.transport.value,
            "enabled": self.enabled,
            "description": self.description,
            "timeout_s": self.timeout_s,
        }
        if self.transport == MCPTransport.STDIO:
            d["command"] = self.command
            d["args"] = self.args
            if self.env:
                d["env"] = self.env
        else:
            d["url"] = self.url
            if self.headers:
                d["headers"] = self.headers
        if self.capabilities:
            d["capabilities"] = self.capabilities
        if self.version:
            d["version"] = self.version
        if self.author:
            d["author"] = self.author
        if self.compatibility:
            d["compatibility"] = self.compatibility
        return d

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> MCPServerConfig:
        """Deserialize from a dict (config.yaml entry or API request body)."""
        transport = MCPTransport(data.get("transport", "stdio"))
        return cls(
            name=data["name"],
            transport=transport,
            command=data.get("command", ""),
            args=data.get("args", []),
            env=data.get("env", {}),
            url=data.get("url", ""),
            headers=data.get("headers", {}),
            enabled=data.get("enabled", True),
            description=data.get("description", ""),
            capabilities=data.get("capabilities", []),
            timeout_s=data.get("timeout_s", 30.0),
            version=data.get("version", ""),
            author=data.get("author", ""),
            compatibility=data.get("compatibility", ""),
        )


@dataclass
class MCPServerState:
    """Runtime state of a connected MCP server."""

    config: MCPServerConfig
    status: MCPServerStatus = MCPServerStatus.DISCONNECTED
    tools: list[MCPToolInfo] = field(default_factory=list)
    error: str | None = None
    server_info: dict[str, Any] = field(default_factory=dict)

    @property
    def name(self) -> str:
        return self.config.name

    @property
    def tool_count(self) -> int:
        return len(self.tools)
