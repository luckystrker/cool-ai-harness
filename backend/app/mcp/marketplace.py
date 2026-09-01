"""MCP Marketplace: integration with the official MCP Registry (Фаза 2 §4).

Queries the official MCP Registry at ``registry.modelcontextprotocol.io`` to
browse, search, and install community MCP servers. The registry is an open,
unauthenticated read-only REST API (v0) that provides standardized server
metadata including install instructions (npm/pypi packages, transport type).

API reference:
    GET https://registry.modelcontextprotocol.io/v0/servers?search=...&limit=...
"""

from __future__ import annotations

from typing import Any

import httpx

from app.core.logging import get_logger
from app.mcp.models import MCPServerConfig, MCPTransport

log = get_logger(__name__)

_REGISTRY_BASE_URL = "https://registry.modelcontextprotocol.io"
_TIMEOUT_S = 15.0


async def search_registry(query: str, *, limit: int = 10) -> list[dict[str, Any]]:
    """Search the official MCP Registry for servers matching a query.

    Returns a list of simplified server summaries suitable for display.
    """
    url = f"{_REGISTRY_BASE_URL}/v0/servers"
    params: dict[str, Any] = {"limit": limit}
    if query.strip():
        params["search"] = query.strip()

    async with httpx.AsyncClient(timeout=_TIMEOUT_S) as client:
        resp = await client.get(url, params=params)
        resp.raise_for_status()
        data = resp.json()

    servers_raw = data.get("servers", [])
    results: list[dict[str, Any]] = []
    for entry in servers_raw:
        server = entry.get("server", {})
        summary = _summarize_entry(server)
        if summary:
            results.append(summary)

    log.info("mcp.marketplace.search", query=query, results=len(results))
    return results


async def get_server_details(registry_name: str) -> dict[str, Any] | None:
    """Fetch full details for a specific server by its registry name.

    Searches the registry for an exact name match.
    """
    url = f"{_REGISTRY_BASE_URL}/v0/servers"
    params: dict[str, Any] = {"search": registry_name, "limit": 20}

    async with httpx.AsyncClient(timeout=_TIMEOUT_S) as client:
        resp = await client.get(url, params=params)
        resp.raise_for_status()
        data = resp.json()

    for entry in data.get("servers", []):
        server = entry.get("server", {})
        if server.get("name") == registry_name:
            return server

    return None


async def list_popular(*, limit: int = 20) -> list[dict[str, Any]]:
    """List recently updated / popular servers from the registry."""
    url = f"{_REGISTRY_BASE_URL}/v0/servers"
    params: dict[str, Any] = {"limit": limit}

    async with httpx.AsyncClient(timeout=_TIMEOUT_S) as client:
        resp = await client.get(url, params=params)
        resp.raise_for_status()
        data = resp.json()

    results: list[dict[str, Any]] = []
    for entry in data.get("servers", []):
        server = entry.get("server", {})
        summary = _summarize_entry(server)
        if summary:
            results.append(summary)

    return results


def registry_entry_to_config(
    entry: dict[str, Any], *, local_name: str = ""
) -> MCPServerConfig | None:
    """Convert a registry server entry into an MCPServerConfig.

    Picks the best installable package (prefers stdio + npx). Returns None if
    no suitable package is found.
    """
    name = entry.get("name", "")
    description = entry.get("description", "")
    packages = entry.get("packages", [])

    if not packages:
        return None

    # Derive a local name from the registry name if not provided.
    if not local_name:
        # e.g. "io.github.modelcontextprotocol/servers-filesystem" -> "filesystem"
        parts = name.rsplit("/", 1)
        local_name = parts[-1] if parts else name
        # Strip common prefixes.
        for prefix in ("servers-", "mcp-server-", "mcp-"):
            if local_name.startswith(prefix):
                local_name = local_name[len(prefix) :]
                break
        # Sanitize.
        local_name = local_name.lower().replace(" ", "-").replace("_", "-")
        local_name = "".join(c for c in local_name if c.isalnum() or c in "-_")

    # Find the best package: prefer stdio with npx runtime hint.
    best_pkg = _pick_best_package(packages)
    if best_pkg is None:
        return None

    transport_info = best_pkg.get("transport", {})
    transport_type = transport_info.get("type", "stdio")
    identifier = best_pkg.get("identifier", "")
    runtime_hint = best_pkg.get("runtimeHint", "")
    registry_type = best_pkg.get("registryType", "npm")

    # Build command + args based on registry type and runtime.
    if registry_type == "npm":
        command = runtime_hint or "npx"
        args = ["-y", identifier]
    elif registry_type == "pypi":
        command = runtime_hint or "uvx"
        args = [identifier]
    else:
        command = runtime_hint or identifier
        args = []

    # Append package arguments (named args with defaults).
    for pkg_arg in best_pkg.get("packageArguments", []):
        if pkg_arg.get("isRequired") and not pkg_arg.get("default"):
            # Skip required args without defaults — user must configure manually.
            continue
        arg_name = pkg_arg.get("name", "")
        default_val = pkg_arg.get("default", "")
        if arg_name and default_val:
            args.extend([f"--{arg_name}", str(default_val)])

    # Determine transport.
    if transport_type in ("sse", "streamable-http", "http"):
        url_template = transport_info.get("url", "http://127.0.0.1:8080/mcp")
        # Replace {port} placeholder with default.
        url = url_template.replace("{port}", "8080")
        return MCPServerConfig(
            name=local_name,
            transport=MCPTransport.HTTP,
            url=url,
            description=description,
            enabled=True,
        )

    # Default: stdio.
    return MCPServerConfig(
        name=local_name,
        transport=MCPTransport.STDIO,
        command=command,
        args=args,
        description=description,
        enabled=True,
    )


# --- Internal helpers ---


def _summarize_entry(server: dict[str, Any]) -> dict[str, Any] | None:
    """Extract a display-friendly summary from a registry server entry."""
    name = server.get("name", "")
    if not name:
        return None

    description = server.get("description", "")
    version = server.get("version", "")
    packages = server.get("packages", [])
    repo = server.get("repository", {})

    # Determine install command and transport from the best package.
    install_command = ""
    transport = ""
    if packages:
        best = _pick_best_package(packages)
        if best:
            registry_type = best.get("registryType", "npm")
            identifier = best.get("identifier", "")
            runtime_hint = best.get("runtimeHint", "")
            transport_info = best.get("transport", {})
            transport = transport_info.get("type", "stdio")

            if registry_type == "npm":
                install_command = f"{runtime_hint or 'npx'} -y {identifier}"
            elif registry_type == "pypi":
                install_command = f"{runtime_hint or 'uvx'} {identifier}"
            else:
                install_command = identifier

    return {
        "name": name,
        "description": description,
        "version": version,
        "repository_url": repo.get("url", ""),
        "install_command": install_command,
        "transport": transport,
        "packages_count": len(packages),
    }


def _pick_best_package(packages: list[dict[str, Any]]) -> dict[str, Any] | None:
    """Pick the best installable package from a list.

    Priority: stdio+npx > stdio > streamable-http > sse > first available.
    """
    if not packages:
        return None

    scored: list[tuple[int, dict[str, Any]]] = []
    for pkg in packages:
        transport = pkg.get("transport", {})
        t_type = transport.get("type", "")
        runtime = pkg.get("runtimeHint", "")
        score = 0
        if t_type == "stdio":
            score += 10
        elif t_type == "streamable-http":
            score += 5
        elif t_type == "sse":
            score += 3
        if runtime == "npx":
            score += 2
        elif runtime == "uvx":
            score += 1
        scored.append((score, pkg))

    scored.sort(key=lambda x: x[0], reverse=True)
    return scored[0][1] if scored else None
