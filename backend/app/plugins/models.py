"""Typed internal model for portable and compatibility plugin bundles."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal

from app.mcp.models import MCPServerConfig
from app.skills.models import Skill

DiagnosticLevel = Literal["info", "warning", "error", "blocker"]
CompatibilityStatus = Literal["supported", "transformed", "ignored", "unsafe"]


@dataclass(frozen=True)
class PluginDiagnostic:
    code: str
    message: str
    level: DiagnosticLevel
    status: CompatibilityStatus
    path: str = ""

    def to_dict(self) -> dict[str, str]:
        return {
            "code": self.code,
            "message": self.message,
            "level": self.level,
            "status": self.status,
            "path": self.path,
        }


@dataclass(frozen=True)
class PluginManifest:
    schema: str
    name: str
    version: str = ""
    description: str = ""
    author: dict[str, str] = field(default_factory=dict)
    homepage: str = ""
    repository: str = ""
    license: str = ""
    keywords: tuple[str, ...] = ()
    extensions: dict[str, dict[str, Any]] = field(default_factory=dict)


@dataclass(frozen=True)
class HookDeclaration:
    hook_id: str
    event: str
    handler_type: Literal["command", "mcp"]
    definition: dict[str, Any]
    matcher: dict[str, Any]
    order: int
    concurrency: Literal["serial", "parallel"]
    capabilities: tuple[str, ...]
    trust_hash: str


@dataclass(frozen=True)
class PluginProvenance:
    source_type: Literal["local", "git", "builtin", "codex", "claude"]
    source: str
    revision: str = ""
    content_hash: str = ""


@dataclass
class PluginBundle:
    root: Path
    manifest: PluginManifest | None
    provenance: PluginProvenance
    skills: list[Skill] = field(default_factory=list)
    mcp_servers: list[MCPServerConfig] = field(default_factory=list)
    hooks: list[HookDeclaration] = field(default_factory=list)
    diagnostics: list[PluginDiagnostic] = field(default_factory=list)

    @property
    def valid(self) -> bool:
        return self.manifest is not None and not any(
            item.level in {"error", "blocker"} and item.path in {"plugin.json", ""}
            for item in self.diagnostics
        )

    @property
    def loadable(self) -> bool:
        return self.manifest is not None and not any(
            item.level == "blocker" for item in self.diagnostics
        )

    @property
    def conformant(self) -> bool:
        portable_violation_codes = {"manifest.unknown_field", "manifest.extensions_ignored"}
        return self.manifest is not None and not any(
            item.code in portable_violation_codes
            or (
                item.level in {"error", "blocker"}
                and not item.path.startswith("io.github.luckystrker.cool/")
            )
            for item in self.diagnostics
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "valid": self.valid,
            "loadable": self.loadable,
            "conformant": self.conformant,
            "root": str(self.root),
            "name": self.manifest.name if self.manifest else None,
            "version": self.manifest.version if self.manifest else None,
            "provenance": {
                "source_type": self.provenance.source_type,
                "source": self.provenance.source,
                "revision": self.provenance.revision,
                "content_hash": self.provenance.content_hash,
            },
            "components": {
                "skills": [skill.name for skill in self.skills],
                "mcp_servers": [server.name for server in self.mcp_servers],
                "hooks": [hook.hook_id for hook in self.hooks],
            },
            "diagnostics": [item.to_dict() for item in self.diagnostics],
        }

    def doctor_dict(self) -> dict[str, Any]:
        report = self.to_dict()
        report["components"] = {
            "skills": [
                {
                    "name": skill.name,
                    "path": str(skill.path),
                    "allowed_tools": skill.tools,
                }
                for skill in self.skills
            ],
            "mcp_servers": [
                {
                    "name": server.name,
                    "transport": server.transport.value,
                    "command": server.command,
                    "args": server.args,
                    "cwd": server.cwd,
                    "url": server.url,
                    "environment_names": sorted(server.env),
                    "header_names": sorted(server.headers),
                    "capabilities": server.capabilities,
                    "plugin_data": server.plugin_data,
                }
                for server in self.mcp_servers
            ],
            "hooks": [
                {
                    "id": hook.hook_id,
                    "event": hook.event,
                    "handler": hook.definition,
                    "matcher": hook.matcher,
                    "order": hook.order,
                    "concurrency": hook.concurrency,
                    "capabilities": list(hook.capabilities),
                    "trust_hash": hook.trust_hash,
                }
                for hook in self.hooks
            ],
        }
        return report
