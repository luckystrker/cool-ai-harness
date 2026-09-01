"""M3 canonical Agent Plugins contract tests."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from app.cli import main
from app.main import _merge_mcp_configs
from app.mcp.client import MCPClient
from app.mcp.models import MCPServerConfig
from app.plugins.compatibility import CompatibilityLoader
from app.plugins.loader import MCP_SCHEMA_ID, PLUGIN_SCHEMA_ID, PluginLoader
from app.plugins.store import PluginStore, PluginStoreError

FIXTURES = Path(__file__).with_name("fixtures") / "plugins"


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
    invalid.write_text(
        invalid.read_text(encoding="utf-8").replace("directory-name", "other-name"),
        encoding="utf-8",
    )

    bundle = PluginLoader().load(root)

    assert bundle.valid is True
    assert bundle.conformant is False
    assert [skill.name for skill in bundle.skills] == ["valid-skill"]
    assert any(item.code == "skill.invalid" for item in bundle.diagnostics)


@pytest.mark.parametrize(
    "extra",
    [
        "license: []",
        "unexpected: true",
        'version: "1.0"',
        "tags: [legacy]",
        "tools: [read_file]",
    ],
)
def test_agent_skill_frontmatter_rejects_nonportable_fields_and_types(
    tmp_path: Path, extra: str
) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    directory = root / "skills" / "strict-skill"
    directory.mkdir(parents=True)
    (directory / "SKILL.md").write_text(
        "---\nname: strict-skill\ndescription: Strict fixture\n" + extra + "\n---\nBody",
        encoding="utf-8",
    )

    bundle = PluginLoader().load(root)

    assert bundle.skills == []
    assert bundle.conformant is False
    assert any(item.code == "skill.invalid" for item in bundle.diagnostics)


@pytest.mark.parametrize(
    "field",
    ['description: " "', 'description: valid\ncompatibility: "   "'],
)
def test_agent_skill_frontmatter_rejects_whitespace_only_text(tmp_path: Path, field: str) -> None:
    root = tmp_path / "plugin"
    root.mkdir()
    _write_json(root / "plugin.json", _manifest())
    directory = root / "skills" / "strict-skill"
    directory.mkdir(parents=True)
    (directory / "SKILL.md").write_text(
        f"---\nname: strict-skill\n{field}\n---\nBody",
        encoding="utf-8",
    )

    bundle = PluginLoader().load(root)

    assert bundle.skills == []
    assert bundle.conformant is False


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


def test_placeholder_expansion_is_single_pass_and_cwd_uses_declared_root(tmp_path: Path) -> None:
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
        {
            "id": "unsafe",
            "event": "Stop",
            "handler": {"type": "command", "command": "../../outside"},
        },
        {"id": "missing-mcp", "event": "Stop", "handler": {"type": "mcp", "server": "server"}},
        {"id": "duplicate", "event": "Stop", "handler": {"type": "command", "command": "python"}},
        {"id": "duplicate", "event": "Stop", "handler": {"type": "command", "command": "python"}},
        {
            "id": "bad-options",
            "event": "Stop",
            "handler": {"type": "command", "command": "python"},
            "order": True,
        },
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
            "mcpServers": {
                "remote": {"type": "streamable-http", "url": "https://example.test/mcp"}
            },
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


def test_representative_portable_and_vendor_fixtures_have_semantic_diagnostics() -> None:
    portable = CompatibilityLoader().load(FIXTURES / "portable-valid")
    partial = CompatibilityLoader().load(FIXTURES / "portable-partial")
    codex = CompatibilityLoader().load(FIXTURES / "codex-declarative")
    claude = CompatibilityLoader().load(FIXTURES / "claude-declarative")
    claude_single = CompatibilityLoader().load(FIXTURES / "claude-single-skill")

    assert portable.conformant is True
    assert [skill.name for skill in partial.skills] == ["healthy-sibling"]
    assert any(item.code == "skill.invalid" for item in partial.diagnostics)
    assert codex.provenance.source_type == "codex"
    assert codex.conformant is False
    assert [skill.name for skill in codex.skills] == ["codex-review"]
    assert {item.status for item in codex.diagnostics} == {"transformed", "ignored"}
    assert claude.provenance.source_type == "claude"
    assert claude.conformant is False
    assert [skill.name for skill in claude.skills] == ["claude-review"]
    assert {item.status for item in claude.diagnostics} == {"transformed", "ignored"}
    assert {item.code for item in codex.diagnostics} >= {
        "compat.codex.hooks",
        "compat.codex.mcp",
    }
    assert {item.code for item in claude.diagnostics} >= {
        "compat.claude.bin",
        "compat.claude.monitors",
        "compat.claude.settings",
        "compat.claude.themes",
    }
    assert claude_single.manifest is not None
    assert claude_single.provenance.source_type == "claude"
    assert [skill.name for skill in claude_single.skills] == ["claude-single-skill"]


def test_store_install_update_disable_remove_and_preserve_data(tmp_path: Path) -> None:
    source = tmp_path / "source"
    source.mkdir()
    _write_json(source / "plugin.json", _manifest("lifecycle.test", version="1.0.0"))
    _skill(source, "lifecycle-skill")
    _write_json(
        source / "mcp.json",
        {
            "$schema": MCP_SCHEMA_ID,
            "mcpServers": {"local": {"type": "stdio", "command": "python"}},
        },
    )
    _write_json(
        source / "io.github.luckystrker.cool" / "hooks" / "hooks.json",
        {
            "version": 1,
            "hooks": [
                {
                    "id": "review-before-enable",
                    "event": "Stop",
                    "handler": {"type": "command", "command": "python"},
                    "capabilities": ["execute"],
                }
            ],
        },
    )
    store = PluginStore(tmp_path / "store")

    installed = store.install_local(source)
    install_path = Path(installed.install_path)
    data_path = Path(installed.data_path)
    (data_path / "user-state.txt").write_text("keep", encoding="utf-8")
    source_skill = source / "skills" / "lifecycle-skill" / "SKILL.md"
    source_skill.write_text(
        source_skill.read_text(encoding="utf-8") + "\nchanged", encoding="utf-8"
    )

    assert "changed" not in (install_path / "skills" / "lifecycle-skill" / "SKILL.md").read_text(
        encoding="utf-8"
    )
    with pytest.raises(PluginStoreError, match="already installed"):
        store.install_local(source)

    assert installed.enabled is False
    assert installed.resolved_dependencies == ["python"]
    assert installed.required_capabilities == ["execute"]
    assert store.load_enabled() == []
    store.set_enabled("lifecycle.test", True)
    assert [bundle.manifest.name for bundle in store.load_enabled() if bundle.manifest] == [
        "lifecycle.test"
    ]

    updated = store.update_local("lifecycle.test", source)
    assert updated.content_hash != installed.content_hash
    assert updated.enabled is False
    assert (data_path / "user-state.txt").read_text(encoding="utf-8") == "keep"
    assert store.load_enabled() == []

    store.set_enabled("lifecycle.test", True)
    store.set_enabled("lifecycle.test", False)
    assert store.load_enabled() == []
    removed = store.remove("lifecycle.test")
    assert removed.name == "lifecycle.test"
    assert data_path.is_dir()
    assert not Path(updated.install_path).exists()
    assert not Path(installed.install_path).exists()


def test_store_purge_requires_explicit_flag(tmp_path: Path) -> None:
    store = PluginStore(tmp_path / "store")
    installed = store.install_local(FIXTURES / "portable-valid")
    data_path = Path(installed.data_path)
    store.remove(installed.name, purge_data=True)
    assert not data_path.exists()


def test_hook_only_runtime_dependency_is_visible_before_enable(tmp_path: Path) -> None:
    source = tmp_path / "hook-only"
    source.mkdir()
    _write_json(source / "plugin.json", _manifest("hook.runtime"))
    _write_json(
        source / "io.github.luckystrker.cool" / "hooks" / "hooks.json",
        {
            "version": 1,
            "hooks": [
                {
                    "id": "hook-runtime",
                    "event": "Stop",
                    "handler": {"type": "command", "command": "hook-runtime-bin"},
                    "capabilities": ["execute"],
                }
            ],
        },
    )

    installed = PluginStore(tmp_path / "store").install_local(source)

    assert installed.enabled is False
    assert installed.resolved_dependencies == ["hook-runtime-bin"]
    assert installed.required_capabilities == ["execute"]


def test_store_integrity_failure_isolated_from_sibling(tmp_path: Path) -> None:
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    _write_json(first / "plugin.json", _manifest("integrity.first"))
    _write_json(second / "plugin.json", _manifest("integrity.second"))
    _skill(first, "first-skill")
    _skill(second, "second-skill")
    store = PluginStore(tmp_path / "store")
    first_entry = store.install_local(first)
    store.install_local(second)
    store.set_enabled("integrity.first", True)
    store.set_enabled("integrity.second", True)
    (Path(first_entry.install_path) / "plugin.json").write_text("{}", encoding="utf-8")

    bundles = store.load_enabled()

    assert len(bundles) == 2
    assert bundles[0].manifest is None
    assert bundles[0].diagnostics[0].code == "store.integrity_error"
    assert bundles[1].manifest is not None
    assert bundles[1].manifest.name == "integrity.second"


def test_lockfile_cannot_redirect_plugin_data_outside_store(tmp_path: Path) -> None:
    store = PluginStore(tmp_path / "store")
    entry = store.install_local(FIXTURES / "portable-valid")
    store.set_enabled(entry.name, True)
    payload = json.loads(store.lock_path.read_text(encoding="utf-8"))
    payload["plugins"][entry.name]["data_path"] = str(tmp_path / "outside")
    store.lock_path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(PluginStoreError, match="data path binding"):
        store.load_enabled()


def test_git_install_requires_and_records_immutable_commit(tmp_path: Path) -> None:
    repository = tmp_path / "repository"
    repository.mkdir()
    _write_json(repository / "plugin.json", _manifest("git.test"))
    _skill(repository, "git-skill")
    subprocess.run(["git", "init", str(repository)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(repository), "add", "."], check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "fixture",
        ],
        check=True,
        capture_output=True,
    )
    revision = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    store = PluginStore(tmp_path / "store")

    with pytest.raises(PluginStoreError, match="full 40-character"):
        store.install_git(str(repository), "main")
    installed = store.install_git(str(repository), revision)

    assert installed.source_type == "git"
    assert installed.revision == revision
    assert store.load("git.test").provenance.revision == revision
    assert installed.enabled is False


def test_cli_manages_default_store_and_doctors_installed_plugin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    from app.core.config import get_settings

    monkeypatch.setenv("DATA_DIR", str(tmp_path / "data"))
    get_settings.cache_clear()
    try:
        assert main(["plugin", "install", str(FIXTURES / "portable-valid")]) == 0
        installed = json.loads(capsys.readouterr().out)
        assert installed["name"] == "fixture.portable"
        assert installed["enabled"] is False
        assert installed["resolved_dependencies"] == ["python"]
        assert installed["required_capabilities"] == ["execute"]

        assert main(["plugin", "list"]) == 0
        assert len(json.loads(capsys.readouterr().out)["plugins"]) == 1
        assert main(["plugin", "enable", "fixture.portable"]) == 0
        assert json.loads(capsys.readouterr().out)["enabled"] is True
        assert main(["plugin", "disable", "fixture.portable"]) == 0
        assert json.loads(capsys.readouterr().out)["enabled"] is False
        assert main(["plugin", "doctor", "fixture.portable"]) == 0
        doctor = json.loads(capsys.readouterr().out)
        assert doctor["name"] == "fixture.portable"
        assert doctor["installation"]["enabled"] is False
        assert doctor["installation"]["resolved_dependencies"] == ["python"]
        assert doctor["installation"]["required_capabilities"] == ["execute"]
    finally:
        get_settings.cache_clear()


def test_enabled_plugin_skills_and_mcp_are_projected_into_runtime(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from app.core.config import get_settings
    from app.skills.registry import SkillRegistry

    monkeypatch.setenv("DATA_DIR", str(tmp_path / "data"))
    get_settings.cache_clear()
    try:
        source = tmp_path / "runtime-plugin"
        source.mkdir()
        _write_json(source / "plugin.json", _manifest("runtime.test"))
        _skill(source, "runtime-skill")
        _write_json(
            source / "mcp.json",
            {
                "$schema": MCP_SCHEMA_ID,
                "mcpServers": {
                    "remote": {
                        "type": "streamable-http",
                        "url": "https://example.test/mcp",
                    }
                },
            },
        )
        store = PluginStore()
        installed = store.install_local(source)
        assert installed.required_capabilities == ["network"]

        registry = SkillRegistry()
        registry.load()

        assert registry.get("runtime-skill") is None
        assert store.enabled_mcp_configs() == []
        store.set_enabled("runtime.test", True)
        registry.load(force=True)
        assert registry.get("runtime-skill") is not None
        assert registry.get("runtime-skill").source == "plugin"  # type: ignore[union-attr]
        config_names = [config.name for config in store.enabled_mcp_configs()]
        assert len(config_names) == 1
        assert config_names[0].startswith("plugin_runtime_test_")
        assert config_names[0].replace("_", "").isalnum()
    finally:
        get_settings.cache_clear()


def test_lockfile_cannot_redirect_removal_to_sibling(tmp_path: Path) -> None:
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    _write_json(first / "plugin.json", _manifest("binding.first"))
    _write_json(second / "plugin.json", _manifest("binding.second"))
    store = PluginStore(tmp_path / "store")
    first_entry = store.install_local(first)
    second_entry = store.install_local(second)
    marker = Path(second_entry.data_path) / "keep.txt"
    marker.write_text("keep", encoding="utf-8")
    payload = json.loads(store.lock_path.read_text(encoding="utf-8"))
    payload["plugins"][first_entry.name]["install_path"] = second_entry.install_path
    payload["plugins"][first_entry.name]["data_path"] = second_entry.data_path
    store.lock_path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(PluginStoreError, match="installation path binding"):
        store.remove(first_entry.name, purge_data=True)

    assert Path(second_entry.install_path).is_dir()
    assert marker.read_text(encoding="utf-8") == "keep"


@pytest.mark.skipif(os.name != "nt", reason="Windows reparse-point behavior")
def test_store_rejects_windows_directory_junction(tmp_path: Path) -> None:
    source = tmp_path / "source"
    outside = tmp_path / "outside"
    source.mkdir()
    outside.mkdir()
    _write_json(source / "plugin.json", _manifest("junction.test"))
    junction = source / "linked-outside"
    result = subprocess.run(
        ["cmd", "/c", "mklink", "/J", str(junction), str(outside)],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        pytest.skip(f"junction creation unavailable: {result.stderr}")

    with pytest.raises(PluginStoreError, match="reparse"):
        PluginStore(tmp_path / "store").install_local(source)


def test_plugin_skill_collision_does_not_replace_builtin(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from app.core.config import get_settings
    from app.skills.registry import SkillRegistry

    monkeypatch.setenv("DATA_DIR", str(tmp_path / "data"))
    get_settings.cache_clear()
    try:
        source = tmp_path / "collision"
        source.mkdir()
        _write_json(source / "plugin.json", _manifest("collision.test"))
        _skill(source, "brainstorm", "Attacker replacement")
        store = PluginStore()
        store.install_local(source)
        store.set_enabled("collision.test", True)

        registry = SkillRegistry()
        registry.load()

        skill = registry.get("brainstorm")
        assert skill is not None
        assert skill.source == "builtin"
        assert skill.description != "Attacker replacement"
    finally:
        get_settings.cache_clear()


def test_mcp_merge_keeps_native_config_on_collision() -> None:
    native = MCPServerConfig(name="same", command="native")
    plugin = MCPServerConfig(name="same", command="plugin")

    merged, collisions = _merge_mcp_configs([native], [plugin])

    assert merged == [native]
    assert collisions == ["same"]


def test_lockfile_enabled_requires_real_boolean(tmp_path: Path) -> None:
    store = PluginStore(tmp_path / "store")
    entry = store.install_local(FIXTURES / "portable-valid")
    payload = json.loads(store.lock_path.read_text(encoding="utf-8"))
    payload["plugins"][entry.name]["enabled"] = "false"
    store.lock_path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(PluginStoreError, match="enabled must be a boolean"):
        store.load_enabled()


@pytest.mark.skipif(os.name != "nt", reason="Windows reparse-point behavior")
def test_compatibility_loader_rejects_junctioned_vendor_metadata(tmp_path: Path) -> None:
    root = tmp_path / "bundle"
    external = tmp_path / "external-metadata"
    root.mkdir()
    external.mkdir()
    _write_json(external / "plugin.json", {"name": "escaped.codex"})
    metadata = root / ".codex-plugin"
    result = subprocess.run(
        ["cmd", "/c", "mklink", "/J", str(metadata), str(external)],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        pytest.skip(f"junction creation unavailable: {result.stderr}")

    bundle = CompatibilityLoader().load(root)

    assert bundle.manifest is None
    assert bundle.diagnostics[0].code == "compat.manifest_symlink"
