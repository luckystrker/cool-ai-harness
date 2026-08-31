"""Fail-closed Agent Plugins 1.0 loader with component-level isolation."""

from __future__ import annotations

import hashlib
import ipaddress
import json
import os
import re
from contextlib import suppress
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import yaml
from jsonschema import Draft202012Validator

from app.mcp.models import MCPServerConfig, MCPTransport
from app.plugins.models import (
    HookDeclaration,
    PluginBundle,
    PluginDiagnostic,
    PluginManifest,
    PluginProvenance,
)
from app.skills.models import Skill

PLUGIN_SCHEMA_ID = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
MCP_SCHEMA_ID = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json"
COOL_NAMESPACE = "io.github.luckystrker.cool"
_MANIFEST_FIELDS = {
    "$schema", "name", "version", "description", "author", "homepage",
    "repository", "license", "keywords", "extensions",
}
_SKILL_NAME = re.compile(r"^(?!.*--)[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
_SKILL_FRONTMATTER = re.compile(r"\A---[ \t]*\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|$)", re.DOTALL)
_PLACEHOLDER = re.compile(r"\$\{(?:PLUGIN_ROOT|PLUGIN_DATA)\}")
_HOOK_ID = re.compile(r"^[a-z0-9](?:[a-z0-9.-]{0,62}[a-z0-9])?$")
_HEADER_NAME = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$")
_INVALID_HEADER_VALUE = re.compile(r"[\x00-\x08\x0a-\x1f\x7f]")
_HOOK_EVENTS = {
    "SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse",
    "PermissionRequest", "PostToolUse", "PreCompact", "PostCompact",
    "SubagentStart", "SubagentStop", "Stop", "Interrupt",
}
_CAPABILITIES = {"read", "write", "execute", "network", "git", "send_external"}


def _schema(name: str) -> dict[str, Any]:
    path = Path(__file__).with_name("schemas") / name
    return json.loads(path.read_text(encoding="utf-8"))


class PluginLoader:
    """Load a directory without executing plugin-controlled code."""

    def __init__(self) -> None:
        self._plugin_validator = Draft202012Validator(_schema("plugin-1.0.0.schema.json"))
        mcp_schema = _schema("mcp-1.0.0.schema.json")
        self._mcp_validator = Draft202012Validator(mcp_schema)
        self._mcp_server_validator = Draft202012Validator(
            {"$ref": "#/$defs/server", "$defs": mcp_schema["$defs"]}
        )

    def load(
        self,
        root: Path,
        *,
        source_type: str = "local",
        source: str | None = None,
        revision: str = "",
        data_root: Path | None = None,
    ) -> PluginBundle:
        root = root.resolve()
        provenance = PluginProvenance(
            source_type=source_type,  # type: ignore[arg-type]
            source=source or str(root),
            revision=revision,
            content_hash=self.content_hash(root) if root.is_dir() else "",
        )
        bundle = PluginBundle(root=root, manifest=None, provenance=provenance)
        if not root.is_dir():
            self._diag(bundle, "plugin.root_missing", "plugin root is not a directory", "blocker")
            return bundle

        manifest_path = root / "plugin.json"
        if not self._contained_file(root, manifest_path):
            self._diag(bundle, "plugin.manifest_missing", "plugin.json is missing or escapes plugin root", "blocker", "plugin.json")
            return bundle
        raw = self._read_json(bundle, manifest_path, fatal=True)
        if not isinstance(raw, dict):
            return bundle

        normalized = dict(raw)
        for field in sorted(set(normalized) - _MANIFEST_FIELDS):
            self._diag(bundle, "manifest.unknown_field", f"unknown top-level field ignored: {field}", "warning", f"plugin.json/{field}", "ignored")
            normalized.pop(field)
        if "extensions" in normalized and not isinstance(normalized["extensions"], dict):
            self._diag(bundle, "manifest.extensions_ignored", "non-object extensions field ignored", "warning", "plugin.json/extensions", "ignored")
            normalized.pop("extensions")

        errors = sorted(self._plugin_validator.iter_errors(normalized), key=lambda e: list(e.path))
        if errors:
            for error in errors:
                self._diag(bundle, "manifest.invalid", error.message, "blocker", self._json_path("plugin.json", error.path), "unsafe")
            return bundle

        bundle.manifest = PluginManifest(
            schema=normalized["$schema"], name=normalized["name"],
            version=normalized.get("version", ""), description=normalized.get("description", ""),
            author=normalized.get("author", {}), homepage=normalized.get("homepage", ""),
            repository=normalized.get("repository", ""), license=normalized.get("license", ""),
            keywords=tuple(normalized.get("keywords", [])), extensions=normalized.get("extensions", {}),
        )
        plugin_data = (data_root or root.parent / ".plugin-data" / bundle.manifest.name).resolve()
        self._load_skills(bundle)
        self._load_mcp(bundle, plugin_data)
        self._load_hooks(bundle)
        return bundle

    def load_builtin_skills(self, skills_root: Path) -> PluginBundle:
        """Project existing Cool skills through the canonical skill model.

        Builtins predate Agent Plugins and therefore have no root plugin.json.
        The synthetic manifest is internal only and is never claimed as a
        portable package; each SKILL.md still passes the same conformance path.
        """
        skills_root = skills_root.resolve()
        bundle = PluginBundle(
            root=skills_root,
            manifest=PluginManifest(schema=PLUGIN_SCHEMA_ID, name="cool-builtin"),
            provenance=PluginProvenance(
                source_type="builtin", source=str(skills_root),
                content_hash=self.content_hash(skills_root),
            ),
        )
        if not skills_root.is_dir():
            self._diag(bundle, "builtin.root_missing", "builtin skill root is missing", "error")
            return bundle
        for skill_dir in sorted(skills_root.iterdir()):
            if not skill_dir.is_dir() or skill_dir.name.startswith((".", "_")):
                continue
            skill_file = skill_dir / "SKILL.md"
            if not self._contained_file(skills_root, skill_file):
                if skill_file.exists() or skill_file.is_symlink():
                    self._diag(bundle, "builtin.skill_path_escape", "builtin SKILL.md escapes its root", "error", f"{skill_dir.name}/SKILL.md", "unsafe")
                continue
            skill = self._load_skill(bundle, skill_dir, skill_file)
            if skill is not None:
                skill.source = "builtin"
                bundle.skills.append(skill)
        return bundle

    def _load_skills(self, bundle: PluginBundle) -> None:
        skills_root = bundle.root / "skills"
        if not skills_root.exists():
            return
        if not self._contained_dir(bundle.root, skills_root):
            self._diag(bundle, "skills.invalid_location", "skills must be a contained directory", "error", "skills", "unsafe")
            return
        for skill_dir in sorted(skills_root.iterdir()):
            if not skill_dir.is_dir():
                continue
            skill_file = skill_dir / "SKILL.md"
            if not self._contained_file(bundle.root, skill_file):
                if skill_file.exists() or skill_file.is_symlink():
                    self._diag(bundle, "skill.path_escape", "SKILL.md escapes plugin root", "error", f"skills/{skill_dir.name}/SKILL.md", "unsafe")
                continue
            skill = self._load_skill(bundle, skill_dir, skill_file)
            if skill is not None:
                bundle.skills.append(skill)

    def _load_skill(self, bundle: PluginBundle, skill_dir: Path, skill_file: Path) -> Skill | None:
        try:
            text = skill_file.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            self._diag(bundle, "skill.unreadable", str(exc), "error", str(skill_file.relative_to(bundle.root)), "unsafe")
            return None
        match = _SKILL_FRONTMATTER.match(text)
        if match is None:
            self._diag(bundle, "skill.frontmatter_missing", "SKILL.md requires YAML frontmatter", "error", str(skill_file.relative_to(bundle.root)))
            return None
        try:
            metadata = yaml.safe_load(match.group(1))
        except yaml.YAMLError as exc:
            self._diag(bundle, "skill.frontmatter_invalid", str(exc), "error", str(skill_file.relative_to(bundle.root)))
            return None
        path = str(skill_file.relative_to(bundle.root)).replace("\\", "/")
        if not isinstance(metadata, dict):
            self._diag(bundle, "skill.frontmatter_invalid", "frontmatter must be a mapping", "error", path)
            return None
        name, description = metadata.get("name"), metadata.get("description")
        problems: list[str] = []
        if not isinstance(name, str) or not 1 <= len(name) <= 64 or not _SKILL_NAME.fullmatch(name):
            problems.append("name must match Agent Skills naming rules")
        elif name != skill_dir.name:
            problems.append("name must match its parent directory")
        if not isinstance(description, str) or not 1 <= len(description) <= 1024:
            problems.append("description must be a non-empty string of at most 1024 characters")
        compatibility = metadata.get("compatibility")
        if compatibility is not None and (not isinstance(compatibility, str) or not 1 <= len(compatibility) <= 500):
            problems.append("compatibility must be a non-empty string of at most 500 characters")
        extra_metadata = metadata.get("metadata")
        if extra_metadata is not None and (
            not isinstance(extra_metadata, dict)
            or any(not isinstance(k, str) or not isinstance(v, str) for k, v in extra_metadata.items())
        ):
            problems.append("metadata must map strings to strings")
        allowed_tools = metadata.get("allowed-tools")
        if allowed_tools is not None and not isinstance(allowed_tools, str):
            problems.append("allowed-tools must be a space-separated string")
        if problems:
            for problem in problems:
                self._diag(bundle, "skill.invalid", problem, "error", path)
            return None
        assert isinstance(name, str)
        assert isinstance(description, str)
        legacy_tags = metadata.get("tags", [])
        legacy_tools = metadata.get("tools", [])
        if not isinstance(legacy_tags, list):
            legacy_tags = []
        if not isinstance(legacy_tools, list):
            legacy_tools = []
        return Skill(
            name=name, description=description, body=text[match.end():].strip(), path=skill_dir,
            source="plugin", tags=[str(value) for value in legacy_tags],
            tools=(allowed_tools.split() if allowed_tools else [str(value) for value in legacy_tools]),
            version=(extra_metadata or {}).get("version", str(metadata.get("version", "1.0"))),
            metadata=metadata,
        )

    def _load_mcp(self, bundle: PluginBundle, data_root: Path) -> None:
        path = bundle.root / "mcp.json"
        if not path.exists():
            return
        if not self._contained_file(bundle.root, path):
            self._diag(bundle, "mcp.invalid_location", "mcp.json must be a contained regular file", "error", "mcp.json", "unsafe")
            return
        raw = self._read_json(bundle, path, fatal=False)
        if not isinstance(raw, dict):
            return
        top_errors = [error for error in self._mcp_validator.iter_errors(raw) if list(error.path)[:1] != ["mcpServers"]]
        if raw.get("$schema") != MCP_SCHEMA_ID or set(raw) != {"$schema", "mcpServers"} or not isinstance(raw.get("mcpServers"), dict) or top_errors:
            self._diag(bundle, "mcp.document_invalid", "mcp.json has invalid schema or top-level fields", "error", "mcp.json")
            return
        assert bundle.manifest is not None
        if bundle.manifest.schema.removesuffix("plugin.schema.json") != raw["$schema"].removesuffix("mcp.schema.json"):
            self._diag(bundle, "mcp.version_mismatch", "mcp.json version does not match plugin.json", "error", "mcp.json")
            return
        for name, server in sorted(raw["mcpServers"].items()):
            errors = list(self._mcp_server_validator.iter_errors(server))
            if errors:
                self._diag(bundle, "mcp.server_invalid", "; ".join(error.message for error in errors), "error", f"mcp.json/mcpServers/{name}")
                continue
            config = self._portable_mcp(bundle, name, server, data_root)
            if config is not None:
                bundle.mcp_servers.append(config)

    def _portable_mcp(self, bundle: PluginBundle, name: str, raw: dict[str, Any], data_root: Path) -> MCPServerConfig | None:
        transport = raw["type"]
        if transport == "sse":
            self._diag(bundle, "mcp.transport_unsupported", "legacy SSE transport is not supported", "warning", f"mcp.json/mcpServers/{name}", "ignored")
            return None
        if transport == "stdio":
            configured_env = raw.get("env", {})
            reserved = {"PLUGIN_ROOT", "PLUGIN_DATA"}
            collides = any(
                (key.upper() in reserved if os.name == "nt" else key in reserved)
                for key in configured_env
            )
            if collides:
                self._diag(bundle, "mcp.env_reserved", "env must not override PLUGIN_ROOT or PLUGIN_DATA under platform name semantics", "error", f"mcp.json/mcpServers/{name}/env", "unsafe")
                return None
            command = raw["command"]
            if command.startswith("./"):
                resolved = (bundle.root / command[2:]).resolve()
                if not resolved.is_relative_to(bundle.root) or not resolved.is_file():
                    self._diag(bundle, "mcp.command_escape", "plugin-relative command is missing or escapes plugin root", "error", f"mcp.json/mcpServers/{name}/command", "unsafe")
                    return None
                command = str(resolved)
            elif "/" in command or "\\" in command:
                self._diag(bundle, "mcp.command_invalid", "command must be a bare executable name or start with ./", "error", f"mcp.json/mcpServers/{name}/command", "unsafe")
                return None
            args = [self._expand(value, bundle.root, data_root) for value in raw.get("args", [])]
            env = {key: self._expand(value, bundle.root, data_root) for key, value in configured_env.items()}
            env["PLUGIN_ROOT"], env["PLUGIN_DATA"] = str(bundle.root), str(data_root)
            raw_cwd = raw.get("cwd", "${PLUGIN_ROOT}")
            cwd = self._expand(raw_cwd, bundle.root, data_root)
            if raw_cwd.startswith("./"):
                cwd_path, allowed_root = (bundle.root / raw_cwd[2:]).resolve(), bundle.root
            elif raw_cwd.startswith("${PLUGIN_DATA}"):
                cwd_path, allowed_root = Path(cwd).resolve(), data_root
            else:
                cwd_path, allowed_root = Path(cwd).resolve(), bundle.root
            if not cwd_path.is_relative_to(allowed_root):
                self._diag(bundle, "mcp.cwd_escape", "cwd escapes its declared PLUGIN_ROOT or PLUGIN_DATA boundary", "error", f"mcp.json/mcpServers/{name}/cwd", "unsafe")
                return None
            return MCPServerConfig(
                name=name,
                transport=MCPTransport.STDIO,
                command=command,
                args=args,
                env=env,
                cwd=str(cwd_path),
                plugin_root=str(bundle.root),
                plugin_data=str(data_root),
            )
        url = raw["url"]
        if not self._valid_remote_url(url):
            self._diag(bundle, "mcp.url_invalid", "remote URL must be absolute HTTP(S), omit credentials and fragments, and use HTTPS outside loopback", "error", f"mcp.json/mcpServers/{name}/url", "unsafe")
            return None
        headers = raw.get("headers", {})
        if not self._valid_headers(headers):
            self._diag(bundle, "mcp.headers_invalid", "header names must be HTTP tokens, values must not contain controls, and names must be unique case-insensitively", "error", f"mcp.json/mcpServers/{name}/headers", "unsafe")
            return None
        return MCPServerConfig(name=name, transport=MCPTransport.HTTP, url=url, headers=headers)

    def _load_hooks(self, bundle: PluginBundle) -> None:
        path = bundle.root / COOL_NAMESPACE / "hooks" / "hooks.json"
        if not path.exists():
            return
        if not self._contained_file(bundle.root, path):
            self._diag(bundle, "hooks.invalid_location", "hooks.json escapes plugin root", "error", f"{COOL_NAMESPACE}/hooks/hooks.json", "unsafe")
            return
        raw = self._read_json(bundle, path, fatal=False)
        if not isinstance(raw, dict) or raw.get("version") != 1 or not isinstance(raw.get("hooks"), list) or set(raw) != {"version", "hooks"}:
            self._diag(bundle, "hooks.document_invalid", "hooks.json must contain version 1 and hooks", "error", f"{COOL_NAMESPACE}/hooks/hooks.json")
            return
        seen_ids: set[str] = set()
        for index, item in enumerate(raw["hooks"]):
            hook = self._parse_hook(bundle, item, index, seen_ids)
            if hook is not None:
                bundle.hooks.append(hook)

    def _parse_hook(
        self, bundle: PluginBundle, item: Any, index: int, seen_ids: set[str]
    ) -> HookDeclaration | None:
        path = f"{COOL_NAMESPACE}/hooks/hooks.json/hooks/{index}"
        allowed_fields = {
            "id", "event", "handler", "matcher", "order", "concurrency", "capabilities"
        }
        if not isinstance(item, dict) or set(item) - allowed_fields:
            self._diag(bundle, "hook.invalid", "hook must use only canonical fields", "error", path)
            return None
        hook_id = item.get("id")
        if (
            not isinstance(hook_id, str)
            or not _HOOK_ID.fullmatch(hook_id)
            or hook_id in seen_ids
            or item.get("event") not in _HOOK_EVENTS
        ):
            self._diag(bundle, "hook.invalid", "hook requires string id and supported event", "error", path)
            return None
        handler = item.get("handler")
        if not isinstance(handler, dict) or handler.get("type") not in {"command", "mcp"}:
            self._diag(bundle, "hook.invalid_handler", "handler type must be command or mcp", "error", path)
            return None
        if not self._valid_hook_handler(bundle.root, handler):
            self._diag(bundle, "hook.invalid_handler", "handler has an invalid shape or command path", "error", path, "unsafe")
            return None
        matcher = item.get("matcher", {})
        order = item.get("order", 0)
        concurrency = item.get("concurrency", "serial")
        if (
            not isinstance(matcher, dict)
            or not isinstance(order, int)
            or isinstance(order, bool)
            or concurrency not in {"serial", "parallel"}
        ):
            self._diag(bundle, "hook.invalid_options", "matcher must be an object, order an integer, and concurrency serial or parallel", "error", path)
            return None
        capabilities = item.get("capabilities", [])
        if (
            not isinstance(capabilities, list)
            or len(set(capabilities)) != len(capabilities)
            or any(value not in _CAPABILITIES for value in capabilities)
        ):
            self._diag(bundle, "hook.invalid_capabilities", "hook declares an unknown capability", "error", path)
            return None
        seen_ids.add(hook_id)
        normalized = {
            "source": bundle.provenance.source,
            "revision": bundle.provenance.revision,
            "content_hash": bundle.provenance.content_hash,
            "id": hook_id,
            "event": item["event"],
            "handler": handler,
            "matcher": matcher,
            "order": order,
            "concurrency": concurrency,
            "capabilities": sorted(capabilities),
        }
        trust_hash = hashlib.sha256(json.dumps(normalized, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        return HookDeclaration(
            hook_id=hook_id, event=item["event"], handler_type=handler["type"],
            definition=handler, matcher=matcher, order=order,
            concurrency=concurrency, capabilities=tuple(capabilities),
            trust_hash=trust_hash,
        )

    @staticmethod
    def _valid_hook_handler(root: Path, handler: dict[str, Any]) -> bool:
        if handler["type"] == "command":
            if set(handler) - {"type", "command", "args", "env"}:
                return False
            command = handler.get("command")
            args, env = handler.get("args", []), handler.get("env", {})
            if (
                not isinstance(command, str)
                or not command
                or not isinstance(args, list)
                or any(not isinstance(value, str) for value in args)
                or not isinstance(env, dict)
                or any(
                    not isinstance(key, str) or not isinstance(value, str)
                    for key, value in env.items()
                )
            ):
                return False
            if command.startswith("./"):
                resolved = (root / command[2:]).resolve()
                return resolved.is_relative_to(root) and resolved.is_file()
            return "/" not in command and "\\" not in command
        if set(handler) - {"type", "server", "tool", "arguments"}:
            return False
        return (
            isinstance(handler.get("server"), str)
            and bool(handler["server"])
            and isinstance(handler.get("tool"), str)
            and bool(handler["tool"])
            and isinstance(handler.get("arguments", {}), dict)
        )

    @staticmethod
    def content_hash(root: Path) -> str:
        digest = hashlib.sha256()
        if not root.is_dir():
            return ""
        for path in sorted(root.rglob("*"), key=lambda p: p.as_posix()):
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                target = path.resolve()
                digest.update(f"L\0{relative}\0{target}\0".encode())
            elif path.is_file():
                digest.update(f"F\0{relative}\0{path.stat().st_size}\0".encode())
                with path.open("rb") as handle:
                    for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                        digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def _expand(value: str, root: Path, data_root: Path) -> str:
        replacements = {"${PLUGIN_ROOT}": str(root), "${PLUGIN_DATA}": str(data_root)}
        return _PLACEHOLDER.sub(lambda match: replacements[match.group(0)], value)

    @staticmethod
    def _valid_remote_url(value: str) -> bool:
        try:
            parsed = urlsplit(value)
            if parsed.scheme not in {"http", "https"} or not parsed.hostname:
                return False
            if parsed.username is not None or parsed.password is not None or parsed.fragment:
                return False
            hostname = parsed.hostname.lower()
            loopback = hostname == "localhost"
            with suppress(ValueError):
                loopback = loopback or ipaddress.ip_address(hostname).is_loopback
            return parsed.scheme == "https" or loopback
        except ValueError:
            return False

    @staticmethod
    def _valid_headers(headers: dict[str, str]) -> bool:
        normalized: set[str] = set()
        for name, value in headers.items():
            lowered = name.lower()
            if (
                not _HEADER_NAME.fullmatch(name)
                or _INVALID_HEADER_VALUE.search(value)
                or lowered in normalized
            ):
                return False
            normalized.add(lowered)
        return True

    @staticmethod
    def _contained_file(root: Path, path: Path) -> bool:
        try:
            return path.resolve().is_relative_to(root) and path.is_file()
        except OSError:
            return False

    @staticmethod
    def _contained_dir(root: Path, path: Path) -> bool:
        try:
            return path.resolve().is_relative_to(root) and path.is_dir()
        except OSError:
            return False

    @staticmethod
    def _json_path(prefix: str, parts: Any) -> str:
        suffix = "/".join(str(part) for part in parts)
        return f"{prefix}/{suffix}" if suffix else prefix

    def _read_json(self, bundle: PluginBundle, path: Path, *, fatal: bool) -> Any:
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            self._diag(bundle, "json.invalid", str(exc), "blocker" if fatal else "error", str(path.relative_to(bundle.root)), "unsafe")
            return None

    @staticmethod
    def _diag(bundle: PluginBundle, code: str, message: str, level: str, path: str = "", status: str = "unsupported") -> None:
        if status == "unsupported":
            status = "unsafe" if level in {"error", "blocker"} else "ignored"
        bundle.diagnostics.append(PluginDiagnostic(code=code, message=message, level=level, status=status, path=path))  # type: ignore[arg-type]
