"""MCP API endpoints (Фаза 2 §4).

Provides REST endpoints for managing MCP server connections:
- List configured servers and their status/tools.
- Add / update / remove server configurations.
- Connect / disconnect / health-check individual servers.
- List all discovered MCP tools across servers.

The frontend settings page and MCP management UI consume these.
"""

from __future__ import annotations

import re
from typing import Any

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field

from app.core.logging import get_logger
from app.mcp.config import rollback_mcp_configs, save_mcp_configs
from app.mcp.models import MCPServerConfig, MCPServerStatus, MCPTransport
from app.mcp.registry import get_mcp_registry
from app.mcp.tool_bridge import refresh_mcp_tools, unregister_mcp_tools

log = get_logger(__name__)

router = APIRouter()

# --- Schemas ---

_SERVER_NAME_RE = re.compile(r"^[a-z0-9]+(?:[-_][a-z0-9]+)*$")


class MCPServerCreate(BaseModel):
    """Request body for adding a new MCP server."""

    name: str = Field(..., min_length=1, max_length=64, description="Unique server identifier")
    transport: str = Field(default="stdio", description="stdio | http")
    command: str = Field(default="", description="Executable for stdio transport")
    args: list[str] = Field(default_factory=list, description="CLI arguments for stdio")
    env: dict[str, str] = Field(default_factory=dict, description="Extra env vars for stdio")
    url: str = Field(default="", description="Endpoint URL for HTTP transport")
    headers: dict[str, str] = Field(default_factory=dict, description="Auth headers for HTTP")
    enabled: bool = Field(default=True)
    description: str = Field(default="", max_length=500)
    capabilities: list[str] = Field(default_factory=list)
    timeout_s: float = Field(default=30.0, ge=1.0, le=300.0)
    version: str = Field(default="", description="Plugin version (semver)")
    author: str = Field(default="", description="Plugin author")
    compatibility: str = Field(default="", description="Minimum harness version")


class MCPServerUpdate(BaseModel):
    """Partial update for an MCP server config."""

    transport: str | None = None
    command: str | None = None
    args: list[str] | None = None
    env: dict[str, str] | None = None
    url: str | None = None
    headers: dict[str, str] | None = None
    enabled: bool | None = None
    description: str | None = None
    capabilities: list[str] | None = None
    timeout_s: float | None = None


class MCPToolOut(BaseModel):
    """A single tool exposed by an MCP server."""

    name: str
    qualified_name: str
    description: str
    server_name: str
    input_schema: dict[str, Any] = Field(default_factory=dict)


class MCPServerOut(BaseModel):
    """Server state as returned by the API."""

    name: str
    transport: str
    status: str
    enabled: bool
    description: str
    command: str = ""
    args: list[str] = Field(default_factory=list)
    url: str = ""
    capabilities: list[str] = Field(default_factory=list)
    timeout_s: float = 30.0
    version: str = ""
    author: str = ""
    compatibility: str = ""
    error: str | None = None
    tools: list[MCPToolOut] = Field(default_factory=list)
    server_info: dict[str, Any] = Field(default_factory=dict)


class MCPServerListResponse(BaseModel):
    servers: list[MCPServerOut]


class MCPToolListResponse(BaseModel):
    tools: list[MCPToolOut]


class MCPConnectResponse(BaseModel):
    name: str
    status: str
    tools_count: int
    error: str | None = None


class MCPHealthResponse(BaseModel):
    name: str
    healthy: bool


# --- Helpers ---


def _validate_server_name(name: str) -> None:
    if not _SERVER_NAME_RE.match(name):
        raise HTTPException(
            status_code=422,
            detail=(
                f"Invalid server name '{name}'. Must be lowercase alphanumeric "
                "with hyphens/underscores (e.g. 'my-server', 'fs_tools')."
            ),
        )


def _state_to_out(state) -> MCPServerOut:
    """Convert an MCPServerState to the API response model."""
    return MCPServerOut(
        name=state.name,
        transport=state.config.transport.value,
        status=state.status.value,
        enabled=state.config.enabled,
        description=state.config.description,
        command=state.config.command,
        args=state.config.args,
        url=state.config.url,
        capabilities=state.config.capabilities,
        timeout_s=state.config.timeout_s,
        version=state.config.version,
        author=state.config.author,
        compatibility=state.config.compatibility,
        error=state.error,
        tools=[
            MCPToolOut(
                name=t.name,
                qualified_name=t.qualified_name,
                description=t.description,
                server_name=t.server_name,
                input_schema=t.input_schema,
            )
            for t in state.tools
        ],
        server_info=state.server_info,
    )


def _persist_configs() -> None:
    """Save current registry configs to config.yaml."""
    registry = get_mcp_registry()
    configs = [s.config for s in registry.list_servers()]
    save_mcp_configs(configs)


# --- Endpoints ---


@router.get("/mcp/servers", response_model=MCPServerListResponse)
async def list_servers() -> MCPServerListResponse:
    """List all configured MCP servers with their status and tools."""
    registry = get_mcp_registry()
    servers = [_state_to_out(s) for s in registry.list_servers()]
    return MCPServerListResponse(servers=servers)


@router.post("/mcp/servers", response_model=MCPServerOut, status_code=201)
async def add_server(req: MCPServerCreate) -> MCPServerOut:
    """Add a new MCP server configuration."""
    _validate_server_name(req.name)

    registry = get_mcp_registry()
    if registry.get_server(req.name) is not None:
        raise HTTPException(status_code=409, detail=f"Server '{req.name}' already exists.")

    try:
        transport = MCPTransport(req.transport)
    except ValueError:
        raise HTTPException(status_code=422, detail=f"Invalid transport '{req.transport}'") from None

    config = MCPServerConfig(
        name=req.name,
        transport=transport,
        command=req.command,
        args=req.args,
        env=req.env,
        url=req.url,
        headers=req.headers,
        enabled=req.enabled,
        description=req.description,
        capabilities=req.capabilities,
        timeout_s=req.timeout_s,
    )
    registry.add_server(config)
    _persist_configs()

    state = registry.get_server(req.name)
    assert state is not None
    log.info("mcp.api.server_added", name=req.name)
    return _state_to_out(state)


@router.patch("/mcp/servers/{name}", response_model=MCPServerOut)
async def update_server(name: str, req: MCPServerUpdate) -> MCPServerOut:
    """Update an MCP server configuration."""
    _validate_server_name(name)
    registry = get_mcp_registry()
    state = registry.get_server(name)
    if state is None:
        raise HTTPException(status_code=404, detail=f"Server '{name}' not found.")

    config = state.config
    if req.transport is not None:
        try:
            config.transport = MCPTransport(req.transport)
        except ValueError:
            raise HTTPException(status_code=422, detail=f"Invalid transport '{req.transport}'") from None
    if req.command is not None:
        config.command = req.command
    if req.args is not None:
        config.args = req.args
    if req.env is not None:
        config.env = req.env
    if req.url is not None:
        config.url = req.url
    if req.headers is not None:
        config.headers = req.headers
    if req.enabled is not None:
        config.enabled = req.enabled
    if req.description is not None:
        config.description = req.description
    if req.capabilities is not None:
        config.capabilities = req.capabilities
    if req.timeout_s is not None:
        config.timeout_s = req.timeout_s

    _persist_configs()
    log.info("mcp.api.server_updated", name=name)
    return _state_to_out(state)


@router.delete("/mcp/servers/{name}", status_code=204)
async def remove_server(name: str) -> None:
    """Remove an MCP server configuration and disconnect it."""
    _validate_server_name(name)
    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        raise HTTPException(status_code=404, detail=f"Server '{name}' not found.")

    # Unregister tools from this server.
    unregister_mcp_tools(server_name=name)
    await registry.disconnect_server(name)
    registry.remove_server(name)
    _persist_configs()
    log.info("mcp.api.server_removed", name=name)


@router.post("/mcp/servers/{name}/connect", response_model=MCPConnectResponse)
async def connect_server(name: str) -> MCPConnectResponse:
    """Connect to an MCP server and discover its tools."""
    _validate_server_name(name)
    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        raise HTTPException(status_code=404, detail=f"Server '{name}' not found.")

    state = await registry.connect_server(name)

    # Register discovered tools in the global tool registry.
    if state.status == MCPServerStatus.CONNECTED:
        refresh_mcp_tools()

    return MCPConnectResponse(
        name=name,
        status=state.status.value,
        tools_count=state.tool_count,
        error=state.error,
    )


@router.post("/mcp/servers/{name}/disconnect", response_model=MCPConnectResponse)
async def disconnect_server(name: str) -> MCPConnectResponse:
    """Disconnect an MCP server and unregister its tools."""
    _validate_server_name(name)
    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        raise HTTPException(status_code=404, detail=f"Server '{name}' not found.")

    unregister_mcp_tools(server_name=name)
    await registry.disconnect_server(name)

    return MCPConnectResponse(name=name, status="disconnected", tools_count=0)


@router.get("/mcp/servers/{name}/health", response_model=MCPHealthResponse)
async def health_check(name: str) -> MCPHealthResponse:
    """Ping an MCP server to check if it's responsive."""
    _validate_server_name(name)
    registry = get_mcp_registry()
    if registry.get_server(name) is None:
        raise HTTPException(status_code=404, detail=f"Server '{name}' not found.")

    healthy = await registry.health_check(name)
    return MCPHealthResponse(name=name, healthy=healthy)


@router.get("/mcp/tools", response_model=MCPToolListResponse)
async def list_tools() -> MCPToolListResponse:
    """List all tools discovered across connected MCP servers."""
    registry = get_mcp_registry()
    tools = registry.all_tools()
    return MCPToolListResponse(
        tools=[
            MCPToolOut(
                name=t.name,
                qualified_name=t.qualified_name,
                description=t.description,
                server_name=t.server_name,
                input_schema=t.input_schema,
            )
            for t in tools
        ]
    )


@router.post("/mcp/reconnect-all", response_model=MCPServerListResponse)
async def reconnect_all() -> MCPServerListResponse:
    """Disconnect and reconnect all enabled servers, refreshing tools."""
    registry = get_mcp_registry()
    await registry.shutdown()
    await registry.connect_all()
    refresh_mcp_tools()
    servers = [_state_to_out(s) for s in registry.list_servers()]
    return MCPServerListResponse(servers=servers)


# --- Marketplace (official MCP Registry) ---


class MCPStoreItem(BaseModel):
    """A server from the official MCP Registry."""

    name: str
    description: str = ""
    version: str = ""
    repository_url: str = ""
    install_command: str = ""
    transport: str = ""
    packages_count: int = 0


class MCPStoreSearchResponse(BaseModel):
    results: list[MCPStoreItem]
    query: str


class MCPStoreInstallRequest(BaseModel):
    """Request body for installing a server from the registry."""

    registry_name: str = Field(..., description="Full registry name (e.g. io.github.user/server)")
    local_name: str = Field(default="", description="Override local server name")


class MCPStoreInstallResponse(BaseModel):
    name: str
    status: str
    tools_count: int = 0
    error: str | None = None


@router.get("/mcp/store/search", response_model=MCPStoreSearchResponse)
async def store_search(q: str = "", limit: int = 10) -> MCPStoreSearchResponse:
    """Search the official MCP Registry for installable servers."""
    from app.mcp.marketplace import search_registry

    try:
        results = await search_registry(q, limit=min(limit, 50))
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"MCP Registry error: {exc}") from exc

    return MCPStoreSearchResponse(
        results=[MCPStoreItem(**r) for r in results],
        query=q,
    )


@router.get("/mcp/store/popular", response_model=MCPStoreSearchResponse)
async def store_popular(limit: int = 20) -> MCPStoreSearchResponse:
    """List popular/recently updated servers from the MCP Registry."""
    from app.mcp.marketplace import list_popular

    try:
        results = await list_popular(limit=min(limit, 50))
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"MCP Registry error: {exc}") from exc

    return MCPStoreSearchResponse(results=[MCPStoreItem(**r) for r in results], query="")


@router.post("/mcp/store/install", response_model=MCPStoreInstallResponse)
async def store_install(req: MCPStoreInstallRequest) -> MCPStoreInstallResponse:
    """Install an MCP server from the official registry (configure + connect)."""
    from app.mcp.marketplace import get_server_details, registry_entry_to_config

    try:
        entry = await get_server_details(req.registry_name)
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"Registry fetch failed: {exc}") from exc

    if entry is None:
        raise HTTPException(status_code=404, detail=f"Server '{req.registry_name}' not found in registry.")

    config = registry_entry_to_config(entry, local_name=req.local_name)
    if config is None:
        raise HTTPException(
            status_code=422,
            detail=f"Server '{req.registry_name}' has no installable packages.",
        )

    registry = get_mcp_registry()
    if registry.get_server(config.name) is not None:
        raise HTTPException(status_code=409, detail=f"Server '{config.name}' already exists locally.")

    registry.add_server(config)
    _persist_configs()

    state = await registry.connect_server(config.name)
    if state.status == MCPServerStatus.CONNECTED:
        refresh_mcp_tools()

    log.info("mcp.api.store_installed", name=config.name, status=state.status.value)
    return MCPStoreInstallResponse(
        name=config.name,
        status=state.status.value,
        tools_count=state.tool_count,
        error=state.error,
    )


# --- Config rollback (Фаза 2 §2 lifecycle) ---


@router.post("/mcp/rollback")
async def post_mcp_rollback() -> dict:
    """Rollback MCP config to the previous version (safe rollback).

    Restores config.yaml.bak over config.yaml, reloads the registry,
    and reconnects servers.
    """
    configs = rollback_mcp_configs()
    if configs is None:
        raise HTTPException(status_code=404, detail="No backup config found")

    registry = get_mcp_registry()
    # Disconnect all current servers.
    await registry.shutdown()
    # Reload from restored config.
    registry.load_configs(configs)
    await registry.connect_all()
    refresh_mcp_tools()

    log.info("mcp.api.rolled_back", servers=len(configs))
    return {"rolled_back": True, "servers": len(configs)}
