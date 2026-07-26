"""Tests for the MCP subsystem (Фаза 2 §4): models, registry, tool bridge, config, API."""

from __future__ import annotations

from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from app.mcp.models import (
    MCPServerConfig,
    MCPServerState,
    MCPServerStatus,
    MCPToolInfo,
    MCPTransport,
)
from app.mcp.registry import MCPRegistry, get_mcp_registry, reset_mcp_registry
from app.mcp.tool_bridge import (
    _build_args_model,
    register_mcp_tools,
    unregister_mcp_tools,
)
from app.tools.base import get_tool

# --- Models ---


class TestMCPModels:
    def test_tool_info_qualified_name(self):
        tool = MCPToolInfo(name="read_file", server_name="filesystem")
        assert tool.qualified_name == "mcp_filesystem_read_file"

    def test_server_config_roundtrip(self):
        config = MCPServerConfig(
            name="test-server",
            transport=MCPTransport.STDIO,
            command="npx",
            args=["-y", "@mcp/server"],
            env={"KEY": "val"},
            enabled=True,
            description="Test server",
            capabilities=["read", "write"],
            timeout_s=15.0,
        )
        d = config.to_dict()
        assert d["name"] == "test-server"
        assert d["transport"] == "stdio"
        assert d["command"] == "npx"
        assert d["args"] == ["-y", "@mcp/server"]
        assert d["env"] == {"KEY": "val"}
        assert d["capabilities"] == ["read", "write"]

        restored = MCPServerConfig.from_dict(d)
        assert restored.name == "test-server"
        assert restored.transport == MCPTransport.STDIO
        assert restored.command == "npx"
        assert restored.args == ["-y", "@mcp/server"]
        assert restored.timeout_s == 15.0

    def test_server_config_http(self):
        config = MCPServerConfig(
            name="remote",
            transport=MCPTransport.HTTP,
            url="http://localhost:8080/mcp",
            headers={"Authorization": "Bearer tok"},
        )
        d = config.to_dict()
        assert d["url"] == "http://localhost:8080/mcp"
        assert d["headers"] == {"Authorization": "Bearer tok"}
        assert "command" not in d

    def test_server_state(self):
        config = MCPServerConfig(name="s1")
        state = MCPServerState(config=config)
        assert state.name == "s1"
        assert state.status == MCPServerStatus.DISCONNECTED
        assert state.tool_count == 0


# --- Registry ---


class TestMCPRegistry:
    def setup_method(self):
        self.registry = MCPRegistry()

    def test_add_and_list(self):
        config = MCPServerConfig(name="srv1", transport=MCPTransport.STDIO, command="echo")
        self.registry.add_server(config)
        servers = self.registry.list_servers()
        assert len(servers) == 1
        assert servers[0].name == "srv1"

    def test_remove_server(self):
        config = MCPServerConfig(name="srv2")
        self.registry.add_server(config)
        assert self.registry.remove_server("srv2") is True
        assert self.registry.remove_server("srv2") is False
        assert len(self.registry.list_servers()) == 0

    def test_list_enabled(self):
        self.registry.add_server(MCPServerConfig(name="on", enabled=True))
        self.registry.add_server(MCPServerConfig(name="off", enabled=False))
        enabled = self.registry.list_enabled()
        assert len(enabled) == 1
        assert enabled[0].name == "on"

    def test_load_configs(self):
        configs = [
            MCPServerConfig(name="a"),
            MCPServerConfig(name="b"),
        ]
        self.registry.load_configs(configs)
        assert len(self.registry.list_servers()) == 2

    @pytest.mark.asyncio
    async def test_connect_server_not_configured(self):
        from app.mcp.client import MCPClientError

        with pytest.raises(MCPClientError, match="not configured"):
            await self.registry.connect_server("nonexistent")

    @pytest.mark.asyncio
    async def test_connect_server_error_sets_status(self):
        config = MCPServerConfig(name="bad", transport=MCPTransport.STDIO, command="nonexistent_cmd_xyz")
        self.registry.add_server(config)
        state = await self.registry.connect_server("bad")
        assert state.status == MCPServerStatus.ERROR
        assert state.error is not None

    def test_all_tools_empty(self):
        assert self.registry.all_tools() == []

    def test_get_tool_not_found(self):
        assert self.registry.get_tool("mcp_x_y") is None


# --- Tool Bridge ---


class TestToolBridge:
    def setup_method(self):
        reset_mcp_registry()

    def teardown_method(self):
        unregister_mcp_tools()
        reset_mcp_registry()

    def test_build_args_model(self):
        tool_info = MCPToolInfo(
            name="search",
            server_name="web",
            input_schema={
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "limit": {"type": "integer", "default": 10},
                },
                "required": ["query"],
            },
        )
        model = _build_args_model(tool_info)
        # Required field should be mandatory.
        instance = model(query="hello")
        assert instance.query == "hello"
        assert instance.limit == 10

    def test_register_and_unregister_mcp_tools(self):
        # Set up a fake connected server in the registry.
        registry = get_mcp_registry()
        config = MCPServerConfig(name="fs", capabilities=["read"])
        registry.add_server(config)
        state = registry.get_server("fs")
        assert state is not None
        state.status = MCPServerStatus.CONNECTED
        state.tools = [
            MCPToolInfo(name="read_file", description="Read a file", server_name="fs",
                        input_schema={"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            MCPToolInfo(name="list_dir", description="List directory", server_name="fs",
                        input_schema={"type": "object", "properties": {}}),
        ]

        count = register_mcp_tools()
        assert count == 2

        # Tools should be in the global registry.
        tool = get_tool("mcp_fs_read_file")
        assert tool is not None
        assert "MCP:fs" in tool.description
        assert tool.capabilities is not None

        tool2 = get_tool("mcp_fs_list_dir")
        assert tool2 is not None

        # Unregister.
        removed = unregister_mcp_tools("fs")
        assert removed == 2
        assert get_tool("mcp_fs_read_file") is None


# --- Config ---


class TestMCPConfig:
    def test_load_from_yaml(self, tmp_path: Path, monkeypatch):
        config_file = tmp_path / "config.yaml"
        config_file.write_text(
            "mcp_servers:\n"
            "  - name: filesystem\n"
            "    transport: stdio\n"
            "    command: npx\n"
            "    args: ['-y', '@mcp/server-filesystem', '/tmp']\n"
            "    enabled: true\n"
            "  - name: remote\n"
            "    transport: http\n"
            "    url: http://localhost:9090/mcp\n",
            encoding="utf-8",
        )
        monkeypatch.setenv("MCP_CONFIG_FILE", str(config_file))

        from app.mcp.config import load_mcp_configs

        configs = load_mcp_configs()
        assert len(configs) == 2
        assert configs[0].name == "filesystem"
        assert configs[0].command == "npx"
        assert configs[1].name == "remote"
        assert configs[1].transport == MCPTransport.HTTP

    def test_load_missing_file(self, tmp_path: Path, monkeypatch):
        monkeypatch.setenv("MCP_CONFIG_FILE", str(tmp_path / "nonexistent.yaml"))
        from app.mcp.config import load_mcp_configs

        configs = load_mcp_configs()
        assert configs == []

    def test_save_and_reload(self, tmp_path: Path, monkeypatch):
        config_file = tmp_path / "config.yaml"
        monkeypatch.setenv("MCP_CONFIG_FILE", str(config_file))

        from app.mcp.config import load_mcp_configs, save_mcp_configs

        configs = [
            MCPServerConfig(name="test-srv", command="echo", args=["hi"]),
        ]
        save_mcp_configs(configs)
        assert config_file.exists()

        loaded = load_mcp_configs()
        assert len(loaded) == 1
        assert loaded[0].name == "test-srv"

    def test_env_interpolation(self, tmp_path: Path, monkeypatch):
        config_file = tmp_path / "config.yaml"
        config_file.write_text(
            "mcp_servers:\n"
            "  - name: auth-server\n"
            "    transport: http\n"
            "    url: http://localhost:8080/mcp\n"
            "    headers:\n"
            "      Authorization: 'Bearer ${TEST_MCP_TOKEN}'\n",
            encoding="utf-8",
        )
        monkeypatch.setenv("MCP_CONFIG_FILE", str(config_file))
        monkeypatch.setenv("TEST_MCP_TOKEN", "secret123")

        from app.mcp.config import load_mcp_configs

        configs = load_mcp_configs()
        assert len(configs) == 1
        assert configs[0].headers["Authorization"] == "Bearer secret123"


# --- API ---


class TestMCPAPI:
    @pytest.fixture
    def client(self):
        from app.main import app

        return TestClient(app)

    def setup_method(self):
        reset_mcp_registry()

    def teardown_method(self):
        reset_mcp_registry()

    def test_list_servers_empty(self, client: TestClient):
        resp = client.get("/api/mcp/servers")
        assert resp.status_code == 200
        data = resp.json()
        assert data["servers"] == []

    def test_add_server(self, client: TestClient):
        resp = client.post("/api/mcp/servers", json={
            "name": "test-fs",
            "transport": "stdio",
            "command": "echo",
            "args": ["hello"],
            "description": "Test filesystem",
        })
        assert resp.status_code == 201
        data = resp.json()
        assert data["name"] == "test-fs"
        assert data["transport"] == "stdio"
        assert data["status"] == "disconnected"

    def test_add_duplicate_server(self, client: TestClient):
        body = {"name": "dup-server", "command": "echo"}
        client.post("/api/mcp/servers", json=body)
        resp = client.post("/api/mcp/servers", json=body)
        assert resp.status_code == 409

    def test_add_invalid_name(self, client: TestClient):
        resp = client.post("/api/mcp/servers", json={"name": "Invalid Name!", "command": "x"})
        assert resp.status_code == 422

    def test_update_server(self, client: TestClient):
        client.post("/api/mcp/servers", json={"name": "upd-srv", "command": "echo"})
        resp = client.patch("/api/mcp/servers/upd-srv", json={"enabled": False, "description": "updated"})
        assert resp.status_code == 200
        data = resp.json()
        assert data["enabled"] is False
        assert data["description"] == "updated"

    def test_update_nonexistent(self, client: TestClient):
        resp = client.patch("/api/mcp/servers/ghost", json={"enabled": False})
        assert resp.status_code == 404

    def test_remove_server(self, client: TestClient):
        client.post("/api/mcp/servers", json={"name": "rm-srv", "command": "echo"})
        resp = client.delete("/api/mcp/servers/rm-srv")
        assert resp.status_code == 204
        # Verify gone.
        resp = client.get("/api/mcp/servers")
        assert all(s["name"] != "rm-srv" for s in resp.json()["servers"])

    def test_remove_nonexistent(self, client: TestClient):
        resp = client.delete("/api/mcp/servers/nope")
        assert resp.status_code == 404

    def test_list_tools_empty(self, client: TestClient):
        resp = client.get("/api/mcp/tools")
        assert resp.status_code == 200
        assert resp.json()["tools"] == []

    def test_health_disconnected(self, client: TestClient):
        client.post("/api/mcp/servers", json={"name": "hc-srv", "command": "echo"})
        resp = client.get("/api/mcp/servers/hc-srv/health")
        assert resp.status_code == 200
        assert resp.json()["healthy"] is False

    def test_disconnect_server(self, client: TestClient):
        client.post("/api/mcp/servers", json={"name": "dc-srv", "command": "echo"})
        resp = client.post("/api/mcp/servers/dc-srv/disconnect")
        assert resp.status_code == 200
        assert resp.json()["status"] == "disconnected"
