"""MCP client: connects to MCP servers via stdio or HTTP (Фаза 2 §4).

Implements the Model Context Protocol client side:
- ``initialize`` handshake (protocol version, capabilities negotiation)
- ``tools/list`` to discover available tools
- ``tools/call`` to invoke a tool on the remote server

Transport layer:
- **stdio**: spawns a subprocess and communicates over stdin/stdout using
  newline-delimited JSON-RPC 2.0 messages.
- **HTTP**: sends JSON-RPC requests to the server's HTTP endpoint (Streamable
  HTTP transport per the MCP spec).

The client is intentionally lightweight — no external MCP SDK dependency.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
from pathlib import Path
from typing import Any

import httpx

from app.core.logging import get_logger
from app.mcp.models import MCPServerConfig, MCPToolInfo, MCPTransport

log = get_logger(__name__)

# MCP protocol version we implement.
_PROTOCOL_VERSION = "2024-11-05"

# Client capabilities advertised during initialization.
_CLIENT_CAPABILITIES: dict[str, Any] = {
    "roots": {"listChanged": False},
}


class MCPClientError(Exception):
    """Raised when the MCP server returns an error or communication fails."""


class MCPClient:
    """Async client for a single MCP server connection.

    Usage::

        client = MCPClient(config)
        await client.connect()
        tools = await client.list_tools()
        result = await client.call_tool("read_file", {"path": "/tmp/x"})
        await client.disconnect()
    """

    def __init__(self, config: MCPServerConfig) -> None:
        self.config = config
        self._request_id = 0
        self._process: asyncio.subprocess.Process | None = None
        self._http_client: httpx.AsyncClient | None = None
        self._connected = False
        self._server_info: dict[str, Any] = {}
        # For stdio: lock to serialize writes to stdin.
        self._write_lock = asyncio.Lock()
        # Pending responses keyed by request id (stdio transport).
        self._pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self._reader_task: asyncio.Task[None] | None = None

    @property
    def connected(self) -> bool:
        return self._connected

    @property
    def server_info(self) -> dict[str, Any]:
        return self._server_info

    # --- Lifecycle ---

    async def connect(self) -> dict[str, Any]:
        """Establish connection and perform the initialize handshake.

        Returns the server's initialize result (capabilities, serverInfo).
        """
        if self._connected:
            return self._server_info

        if self.config.transport == MCPTransport.STDIO:
            await self._connect_stdio()
        else:
            await self._connect_http()

        # Perform the initialize handshake.
        result = await self._request(
            "initialize",
            {
                "protocolVersion": _PROTOCOL_VERSION,
                "capabilities": _CLIENT_CAPABILITIES,
                "clientInfo": {
                    "name": "cool-ai-harness",
                    "version": "0.1.0",
                },
            },
        )
        self._server_info = result
        self._connected = True

        # Send initialized notification.
        await self._notify("notifications/initialized", {})

        log.info(
            "mcp.connected",
            server=self.config.name,
            transport=self.config.transport.value,
            server_name=result.get("serverInfo", {}).get("name", "unknown"),
        )
        return result

    async def disconnect(self) -> None:
        """Gracefully shut down the connection."""
        self._connected = False

        if self._reader_task and not self._reader_task.done():
            self._reader_task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await self._reader_task
            self._reader_task = None

        if self._process:
            try:
                self._process.terminate()
                await asyncio.wait_for(self._process.wait(), timeout=5.0)
            except (ProcessLookupError, TimeoutError, OSError):
                with contextlib.suppress(ProcessLookupError, OSError):
                    self._process.kill()
            self._process = None

        if self._http_client:
            await self._http_client.aclose()
            self._http_client = None

        # Cancel any pending futures.
        for future in self._pending.values():
            if not future.done():
                future.cancel()
        self._pending.clear()

        log.info("mcp.disconnected", server=self.config.name)

    # --- MCP Operations ---

    async def list_tools(self) -> list[MCPToolInfo]:
        """Discover tools exposed by the MCP server."""
        result = await self._request("tools/list", {})
        tools: list[MCPToolInfo] = []
        for item in result.get("tools", []):
            tools.append(
                MCPToolInfo(
                    name=item.get("name", ""),
                    description=item.get("description", ""),
                    input_schema=item.get("inputSchema", {}),
                    server_name=self.config.name,
                )
            )
        log.info("mcp.tools_listed", server=self.config.name, count=len(tools))
        return tools

    async def call_tool(self, tool_name: str, arguments: dict[str, Any] | None = None) -> Any:
        """Invoke a tool on the MCP server and return the result content."""
        result = await self._request(
            "tools/call",
            {
                "name": tool_name,
                "arguments": arguments or {},
            },
        )
        # MCP tool results have a "content" array with typed content blocks.
        content = result.get("content", [])
        is_error = result.get("isError", False)

        # Extract text content for the agent loop.
        texts: list[str] = []
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                texts.append(block.get("text", ""))
            elif isinstance(block, str):
                texts.append(block)

        output = "\n".join(texts) if texts else json.dumps(result, default=str)

        if is_error:
            raise MCPClientError(f"MCP tool '{tool_name}' error: {output}")
        return output

    async def ping(self) -> bool:
        """Health-check: send a ping request."""
        try:
            await self._request("ping", {})
            return True
        except Exception:
            return False

    # --- stdio transport ---

    async def _connect_stdio(self) -> None:
        """Spawn the MCP server subprocess."""
        if not self.config.command:
            raise MCPClientError(f"Server '{self.config.name}': no command configured for stdio")

        env = {**os.environ, **self.config.env}
        if self.config.plugin_data:
            plugin_data = Path(self.config.plugin_data).resolve()
            plugin_data.mkdir(parents=True, exist_ok=True)
            if self.config.cwd:
                cwd = Path(self.config.cwd).resolve()
                if cwd.is_relative_to(plugin_data):
                    cwd.mkdir(parents=True, exist_ok=True)
        # Strip secret-looking env vars if sandbox_strip_env is enabled.
        from app.core.config import get_settings

        settings = get_settings()
        if settings.sandbox_strip_env:
            env = _strip_secrets_from_env(env)

        cmd = [self.config.command, *self.config.args]
        log.info("mcp.spawning", server=self.config.name, command=" ".join(cmd))

        try:
            self._process = await asyncio.create_subprocess_exec(
                *cmd,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                env=env,
                cwd=self.config.cwd or None,
            )
        except (OSError, FileNotFoundError) as exc:
            raise MCPClientError(f"Failed to spawn MCP server '{self.config.name}': {exc}") from exc

        # Start background reader for stdout.
        self._reader_task = asyncio.create_task(self._stdio_reader())

    async def _stdio_reader(self) -> None:
        """Read newline-delimited JSON-RPC messages from the subprocess stdout."""
        assert self._process is not None
        assert self._process.stdout is not None
        try:
            while True:
                line = await self._process.stdout.readline()
                if not line:
                    break  # EOF — process exited.
                line_str = line.decode("utf-8", errors="replace").strip()
                if not line_str:
                    continue
                try:
                    msg = json.loads(line_str)
                except json.JSONDecodeError:
                    log.debug("mcp.stdio.non_json", server=self.config.name, line=line_str[:200])
                    continue
                self._dispatch_message(msg)
        except asyncio.CancelledError:
            pass
        except Exception as exc:
            log.error("mcp.stdio.reader_error", server=self.config.name, error=str(exc))

    def _dispatch_message(self, msg: dict[str, Any]) -> None:
        """Route an incoming JSON-RPC message to the pending future."""
        msg_id = msg.get("id")
        if msg_id is not None and msg_id in self._pending:
            future = self._pending.pop(msg_id)
            if not future.done():
                if "error" in msg:
                    future.set_exception(
                        MCPClientError(
                            f"RPC error {msg['error'].get('code', '?')}: "
                            f"{msg['error'].get('message', 'unknown')}"
                        )
                    )
                else:
                    future.set_result(msg.get("result", {}))

    async def _stdio_send(self, message: dict[str, Any]) -> None:
        """Write a JSON-RPC message to the subprocess stdin."""
        assert self._process is not None
        assert self._process.stdin is not None
        data = json.dumps(message) + "\n"
        async with self._write_lock:
            self._process.stdin.write(data.encode("utf-8"))
            await self._process.stdin.drain()

    # --- HTTP transport ---

    async def _connect_http(self) -> None:
        """Initialize the HTTP client for the MCP server."""
        if not self.config.url:
            raise MCPClientError(f"Server '{self.config.name}': no URL configured for HTTP")
        client_generated = {
            "accept",
            "accept-encoding",
            "connection",
            "content-length",
            "content-type",
            "host",
            "user-agent",
        }
        plugin_headers = {
            name: value
            for name, value in self.config.headers.items()
            if name.lower() not in client_generated
        }
        self._http_client = httpx.AsyncClient(
            base_url=self.config.url,
            headers={
                **plugin_headers,
                "Content-Type": "application/json",
            },
            timeout=httpx.Timeout(self.config.timeout_s),
        )

    # --- JSON-RPC plumbing ---

    def _next_id(self) -> int:
        self._request_id += 1
        return self._request_id

    async def _request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        """Send a JSON-RPC request and await the response."""
        msg_id = self._next_id()
        message = {
            "jsonrpc": "2.0",
            "id": msg_id,
            "method": method,
            "params": params,
        }

        if self.config.transport == MCPTransport.STDIO:
            return await self._stdio_request(msg_id, message)
        else:
            return await self._http_request(message)

    async def _stdio_request(self, msg_id: int, message: dict[str, Any]) -> dict[str, Any]:
        """Send via stdio and wait for the matching response."""
        loop = asyncio.get_running_loop()
        future: asyncio.Future[dict[str, Any]] = loop.create_future()
        self._pending[msg_id] = future
        await self._stdio_send(message)
        try:
            return await asyncio.wait_for(future, timeout=self.config.timeout_s)
        except TimeoutError:
            self._pending.pop(msg_id, None)
            raise MCPClientError(
                f"MCP server '{self.config.name}' timed out after {self.config.timeout_s}s"
            ) from None

    async def _http_request(self, message: dict[str, Any]) -> dict[str, Any]:
        """Send via HTTP POST and parse the JSON-RPC response."""
        assert self._http_client is not None
        try:
            resp = await self._http_client.post("", json=message)
            resp.raise_for_status()
        except httpx.HTTPStatusError as exc:
            raise MCPClientError(
                f"MCP HTTP error {exc.response.status_code}: {exc.response.text[:200]}"
            ) from exc
        except httpx.HTTPError as exc:
            raise MCPClientError(f"MCP HTTP request failed: {exc}") from exc

        data = resp.json()
        if "error" in data:
            err = data["error"]
            raise MCPClientError(
                f"RPC error {err.get('code', '?')}: {err.get('message', 'unknown')}"
            )
        return data.get("result", {})

    async def _notify(self, method: str, params: dict[str, Any]) -> None:
        """Send a JSON-RPC notification (no response expected)."""
        message = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }
        if self.config.transport == MCPTransport.STDIO:
            await self._stdio_send(message)
        elif self._http_client:
            with contextlib.suppress(httpx.HTTPError):
                await self._http_client.post("", json=message)


# --- Helpers ---


def _strip_secrets_from_env(env: dict[str, str]) -> dict[str, str]:
    """Remove environment variables that look like secrets."""
    secret_patterns = ("KEY", "SECRET", "TOKEN", "PASSWORD", "CREDENTIAL")
    return {k: v for k, v in env.items() if not any(pat in k.upper() for pat in secret_patterns)}
