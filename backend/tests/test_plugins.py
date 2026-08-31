"""M3 canonical Agent Plugins contract tests."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from app.cli import main
from app.mcp.client import MCPClient
from app.mcp.models import MCPServerConfig
from app.plugins.loader import MCP_SCHEMA_ID, PLUGIN_SCHEMA_ID, PluginLoader


def _write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def _manifest(name: str = "portable.test", **extra: object) -> dict[str, object]:
    return {"$schema": PLUGIN_SCHEMA_ID, "name": name, **extra}


def _skill(root: Path, name: str, description: str = "A conforming test skill.") -> None:
    directory = root / "skills" / name
    directory.mkdir(parents=True)
    (directory / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {description}\n---\n\nUse this skill.",
        encoding="utf-8",
    )


def test_minimal_manifest_and_unknown_field_failure_boundary(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest(unknownVendorField=True))
    _skill(root, "portable-skill")

    bundle = PluginLoader().load(root)

    assert bundle.valid is True
    assert bundle.conformant is False
    assert [skill.name for skill in bundle.skills] == ["portable-skill"]
    assert [(item.code, item.status) for item in bundle.diagnostics] == [
        ("manifest.unknown_field", "ignored")
    ]


def test_skill_frontmatter_delimiters_ignore_dashes_inside_yaml(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    directory = root / "skills" / "compare-sections"
    directory.mkdir(parents=True)
    (directory / "SKILL.md").write_text(
        '---\nname: compare-sections\ndescription: "Compare --- separated sections"\n---\nBody',
        encoding="utf-8",
    )

    bundle = PluginLoader().load(root)

    assert [skill.name for skill in bundle.skills] == ["compare-sections"]


def test_fatal_manifest_error_prevents_component_discovery(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", {"$schema": PLUGIN_SCHEMA_ID, "name": "Bad Name"})
    _skill(root, "must-not-load")

    bundle = PluginLoader().load(root)

    assert bundle.valid is False
    assert bundle.loadable is False
    assert bundle.skills == []
    assert {item.code for item in bundle.diagnostics} == {"manifest.invalid"}


def test_invalid_skill_isolated_from_valid_sibling(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    _skill(root, "valid-skill")
    _skill(root, "directory-name")
    invalid = root / "skills" / "directory-name" / "SKILL.md"
    invalid.write_text(invalid.read_text(encoding="utf-8").replace("directory-name", "other-name"), encoding="utf-8")

    bundle = PluginLoader().load(root)

    assert bundle.valid is True
    assert bundle.conformant is False
    assert [skill.name for skill in bundle.skills] == ["valid-skill"]
    assert any(item.code == "skill.invalid" for item in bundle.diagnostics)


def test_mcp_entries_are_isolated_and_placeholders_expand_once(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    executable = root / "bin" / "server"
    executable.parent.mkdir()
    executable.write_text("fixture", encoding="utf-8")
    _write_json(
        root / "mcp.json",
        {
            "$schema": MCP_SCHEMA_ID,
            "mcpServers": {
                "local": {
                    "type": "stdio",
                    "command": "./bin/server",
                    "args": ["${PLUGIN_ROOT}/config", "${UNKNOWN}"],
                    "env": {"STATE": "${PLUGIN_DATA}/state"},
                    "cwd": "${PLUGIN_DATA}/work",
                },
                "remote": {"type": "streamable-http", "url": "https://example.test/mcp"},
                "insecure-remote": {"type": "streamable-http", "url": "http://example.test/mcp"},
                "relative-cwd": {"type": "stdio", "command": "python", "cwd": "./runtime"},
                "broken": {"type": "stdio", "command": "../escape"},
            },
        },
    )
    data_root = tmp_path / "data"

    bundle = PluginLoader().load(root, data_root=data_root)

    assert [server.name for server in bundle.mcp_servers] == ["local", "relative-cwd", "remote"]
    local = bundle.mcp_servers[0]
    assert local.command == str(executable.resolve())
    assert Path(local.args[0]).resolve() == root.resolve() / "config"
    assert local.args[1] == "${UNKNOWN}"
    assert local.env["PLUGIN_ROOT"] == str(root.resolve())
    assert local.env["PLUGIN_DATA"] == str(data_root.resolve())
    assert local.cwd == str(data_root.resolve() / "work")
    assert bundle.mcp_servers[1].cwd == str(root.resolve() / "runtime")
    assert any(item.code == "mcp.command_invalid" for item in bundle.diagnostics)
    assert any(item.code == "mcp.url_invalid" for item in bundle.diagnostics)


def test_remote_mcp_url_allows_http_only_for_loopback(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    _write_json(
        root / "mcp.json",
        {
            "$schema": MCP_SCHEMA_ID,
            "mcpServers": {
                "local": {"type": "streamable-http", "url": "http://127.0.0.1:9000/mcp"},
                "subdomain": {"type": "streamable-http", "url": "http://fake.localhost/mcp"},
                "fragment": {"type": "streamable-http", "url": "https://example.test/mcp#secret"},
                "userinfo": {"type": "streamable-http", "url": "https://user@example.test/mcp"},
                "headers": {
                    "type": "streamable-http",
                    "url": "https://example.test/mcp",
                    "headers": {"X-Test": "one", "x-test": "two"},
                },
            },
        },
    )

    bundle = PluginLoader().load(root)

    assert [server.name for server in bundle.mcp_servers] == ["local"]
    assert [item.code for item in bundle.diagnostics].count("mcp.url_invalid") == 3
    assert [item.code for item in bundle.diagnostics].count("mcp.headers_invalid") == 1


def test_placeholder_expansion_is_single_pass_and_cwd_uses_declared_root(
    tmp_path: Path
) -> None:
    root = tmp_path / "literal-${PLUGIN_DATA}"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    _write_json(
        root / "mcp.json",
        {
            "$schema": MCP_SCHEMA_ID,
            "mcpServers": {
                "single-pass": {
                    "type": "stdio",
                    "command": "python",
                    "args": ["${PLUGIN_ROOT}"],
                    "cwd": "${PLUGIN_ROOT}/../data",
                }
            },
        },
    )

    data_root = tmp_path / "data"
    assert PluginLoader._expand("${PLUGIN_ROOT}", root.resolve(), data_root.resolve()) == str(
        root.resolve()
    )
    bundle = PluginLoader().load(root, data_root=data_root)

    assert bundle.mcp_servers == []
    assert any(item.code == "mcp.cwd_escape" for item in bundle.diagnostics)


def test_hook_trust_hash_covers_definition_matcher_and_source(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    hook_path = root / "io.github.luckystrker.cool" / "hooks" / "hooks.json"
    hook = {
        "id": "format-before-write",
        "event": "PreToolUse",
        "handler": {"type": "command", "command": "formatter", "args": ["--check"]},
        "matcher": {"tool": "write_file"},
        "capabilities": ["execute", "read"],
    }
    _write_json(hook_path, {"version": 1, "hooks": [hook]})
    loader = PluginLoader()

    first = loader.load(root, source="local-a").hooks[0].trust_hash
    hook["matcher"] = {"tool": "bash"}
    hook["capabilities"] = ["execute", "write"]
    hook["order"] = 10
    hook["concurrency"] = "parallel"
    _write_json(hook_path, {"version": 1, "hooks": [hook]})
    second = loader.load(root, source="local-a").hooks[0].trust_hash
    third = loader.load(root, source="local-b").hooks[0].trust_hash

    assert len(first) == 64
    assert len({first, second, third}) == 3


def test_hooks_reject_unsafe_handlers_duplicate_ids_and_invalid_options(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    hook_path = root / "io.github.luckystrker.cool" / "hooks" / "hooks.json"
    hooks = [
        {"id": "unsafe", "event": "Stop", "handler": {"type": "command", "command": "../../outside"}},
        {"id": "missing-mcp", "event": "Stop", "handler": {"type": "mcp", "server": "server"}},
        {"id": "duplicate", "event": "Stop", "handler": {"type": "command", "command": "python"}},
        {"id": "duplicate", "event": "Stop", "handler": {"type": "command", "command": "python"}},
        {"id": "bad-options", "event": "Stop", "handler": {"type": "command", "command": "python"}, "order": True},
        {"id": "recover", "event": "Stop", "handler": {"type": "mcp", "server": "server"}},
        {"id": "recover", "event": "Stop", "handler": {"type": "command", "command": "python"}},
    ]
    _write_json(hook_path, {"version": 1, "hooks": hooks})

    bundle = PluginLoader().load(root)

    assert [hook.hook_id for hook in bundle.hooks] == ["duplicate", "recover"]
    assert bundle.conformant is True  # Cool-only hooks do not change Tier-1 conformance.
    assert bundle.loadable is True
    assert len([item for item in bundle.diagnostics if item.level == "error"]) == 5


@pytest.mark.skipif(os.name != "nt", reason="Windows environment names are case-insensitive")
def test_windows_rejects_case_insensitive_reserved_mcp_env_names(tmp_path: Path) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    _write_json(
        root / "mcp.json",
        {
            "$schema": MCP_SCHEMA_ID,
            "mcpServers": {
                "ambiguous": {
                    "type": "stdio",
                    "command": "python",
                    "env": {"Plugin_Data": "attacker-selected"},
                }
            },
        },
    )

    bundle = PluginLoader().load(root)

    assert bundle.mcp_servers == []
    assert any(item.code == "mcp.env_reserved" for item in bundle.diagnostics)


async def test_plugin_data_and_nested_cwd_created_before_stdio_spawn(tmp_path: Path) -> None:
    data_root = tmp_path / "fresh-data"
    cwd = data_root / "runtime"
    client = MCPClient(
        MCPServerConfig(
            name="data-root-smoke",
            command=sys.executable,
            args=["-c", "import time; time.sleep(10)"],
            cwd=str(cwd),
            plugin_root=str(tmp_path),
            plugin_data=str(data_root),
        )
    )

    await client._connect_stdio()
    try:
        assert data_root.is_dir()
        assert cwd.is_dir()
    finally:
        await client.disconnect()


def test_existing_cool_skills_use_canonical_validation() -> None:
    repo = Path(__file__).resolve().parents[2]
    bundle = PluginLoader().load_builtin_skills(repo / "skills")

    assert bundle.manifest is not None
    assert bundle.manifest.name == "cool-builtin"
    assert [skill.name for skill in bundle.skills] == [
        "brainstorm",
        "code-task",
        "deep-research",
        "skill-creation",
        "summarize-document",
        "translate",
    ]
    assert bundle.conformant is True


def test_plugin_cli_validate_and_doctor_emit_machine_readable_report(
    tmp_path: Path, capsys
) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest("cli.test"))
    _skill(root, "doctor-skill")
    _write_json(
        root / "mcp.json",
        {
            "$schema": MCP_SCHEMA_ID,
            "mcpServers": {"remote": {"type": "streamable-http", "url": "https://example.test/mcp"}},
        },
    )
    _write_json(
        root / "io.github.luckystrker.cool" / "hooks" / "hooks.json",
        {
            "version": 1,
            "hooks": [
                {
                    "id": "doctor-hook",
                    "event": "Stop",
                    "handler": {"type": "command", "command": "python"},
                    "matcher": {"reason": "complete"},
                    "capabilities": ["execute"],
                }
            ],
        },
    )

    assert main(["plugin", "validate", str(root)]) == 0
    validate = json.loads(capsys.readouterr().out)
    assert validate["conformant"] is True
    assert validate["name"] == "cli.test"

    assert main(["plugin", "doctor", str(root)]) == 0
    doctor = json.loads(capsys.readouterr().out)
    assert doctor["provenance"]["content_hash"]
    assert doctor["components"]["mcp_servers"][0]["transport"] == "http"
    hook = doctor["components"]["hooks"][0]
    assert hook["handler"] == {"type": "command", "command": "python"}
    assert hook["capabilities"] == ["execute"]
    assert len(hook["trust_hash"]) == 64
