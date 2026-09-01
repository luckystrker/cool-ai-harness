"""Content-addressed plugin installation store and atomic lockfile lifecycle."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
from contextlib import suppress
from dataclasses import asdict, dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Literal

from app.core.config import get_settings
from app.mcp.models import MCPServerConfig, MCPTransport
from app.plugins.loader import PluginLoader
from app.plugins.models import PluginBundle, PluginDiagnostic

_COMMIT_SHA = re.compile(r"^[0-9a-fA-F]{40}$")


class PluginStoreError(RuntimeError):
    """A fail-closed plugin lifecycle error."""


@dataclass
class PluginLockEntry:
    name: str
    version: str
    enabled: bool
    source_type: Literal["local", "git"]
    source: str
    revision: str
    content_hash: str
    install_path: str
    data_path: str
    installed_at: str
    diagnostics: list[dict[str, str]] = field(default_factory=list)
    resolved_dependencies: list[str] = field(default_factory=list)
    required_capabilities: list[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> PluginLockEntry:
        return cls(**value)


class PluginStore:
    """Own immutable installs while preserving mutable data across updates/removal."""

    def __init__(self, root: Path | None = None) -> None:
        base = (root or get_settings().data_dir / "plugins").resolve()
        self.root = base
        self.installs_dir = base / "installations"
        self.data_dir = base / "data"
        self.lock_path = base / "plugins.lock.json"
        self.loader = PluginLoader()

    def install_local(self, source: Path) -> PluginLockEntry:
        if self._is_link_like(source):
            raise PluginStoreError("plugin source root must not be a symlink or reparse point")
        return self._install_from_directory(
            source.resolve(), source_type="local", source=str(source.resolve())
        )

    def update_local(self, name: str, source: Path) -> PluginLockEntry:
        if self.get(name) is None:
            raise PluginStoreError(f"plugin is not installed: {name}")
        if self._is_link_like(source):
            raise PluginStoreError("plugin source root must not be a symlink or reparse point")
        return self._install_from_directory(
            source.resolve(),
            source_type="local",
            source=str(source.resolve()),
            expected_name=name,
            replacing=True,
        )

    def install_git(self, source: str, revision: str) -> PluginLockEntry:
        if not _COMMIT_SHA.fullmatch(revision):
            raise PluginStoreError("Git revision must be a full 40-character commit SHA")
        self._validate_git_source(source)
        self.root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="git-", dir=self.root) as temporary:
            checkout = Path(temporary) / "checkout"
            self._git("clone", "--no-checkout", source, str(checkout))
            try:
                self._git("-C", str(checkout), "checkout", "--detach", revision)
            except PluginStoreError:
                self._git("-C", str(checkout), "fetch", "--depth", "1", "origin", revision)
                self._git("-C", str(checkout), "checkout", "--detach", revision)
            resolved = self._git("-C", str(checkout), "rev-parse", "HEAD").strip().lower()
            if resolved != revision.lower():
                raise PluginStoreError(
                    f"Git checkout mismatch: expected {revision}, got {resolved}"
                )
            return self._install_from_directory(
                checkout, source_type="git", source=source, revision=resolved
            )

    def update_git(self, name: str, source: str, revision: str) -> PluginLockEntry:
        current = self.get(name)
        if current is None:
            raise PluginStoreError(f"plugin is not installed: {name}")
        if not _COMMIT_SHA.fullmatch(revision):
            raise PluginStoreError("Git revision must be a full 40-character commit SHA")
        self._validate_git_source(source)
        self.root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="git-", dir=self.root) as temporary:
            checkout = Path(temporary) / "checkout"
            self._git("clone", "--no-checkout", source, str(checkout))
            try:
                self._git("-C", str(checkout), "checkout", "--detach", revision)
            except PluginStoreError:
                self._git("-C", str(checkout), "fetch", "--depth", "1", "origin", revision)
                self._git("-C", str(checkout), "checkout", "--detach", revision)
            resolved = self._git("-C", str(checkout), "rev-parse", "HEAD").strip().lower()
            if resolved != revision.lower():
                raise PluginStoreError(
                    f"Git checkout mismatch: expected {revision}, got {resolved}"
                )
            return self._install_from_directory(
                checkout,
                source_type="git",
                source=source,
                revision=resolved,
                expected_name=name,
                replacing=True,
            )

    def list_installed(self) -> list[PluginLockEntry]:
        return sorted(self._read_entries().values(), key=lambda entry: entry.name)

    def get(self, name: str) -> PluginLockEntry | None:
        return self._read_entries().get(name)

    def set_enabled(self, name: str, enabled: bool) -> PluginLockEntry:
        entries = self._read_entries()
        entry = entries.get(name)
        if entry is None:
            raise PluginStoreError(f"plugin is not installed: {name}")
        if enabled:
            self._load_locked(entry)
        entry.enabled = enabled
        entries[name] = entry
        self._write_entries(entries)
        return entry

    def load(self, name: str) -> PluginBundle:
        entry = self.get(name)
        if entry is None:
            raise PluginStoreError(f"plugin is not installed: {name}")
        return self._load_locked(entry)

    def doctor(self, name: str) -> dict[str, Any]:
        entry = self.get(name)
        if entry is None:
            raise PluginStoreError(f"plugin is not installed: {name}")
        report = self._load_locked(entry).doctor_dict()
        report["installation"] = {
            "enabled": entry.enabled,
            "resolved_dependencies": entry.resolved_dependencies,
            "required_capabilities": entry.required_capabilities,
            "install_path": entry.install_path,
            "data_path": entry.data_path,
        }
        return report

    def remove(self, name: str, *, purge_data: bool = False) -> PluginLockEntry:
        entries = self._read_entries()
        entry = entries.pop(name, None)
        if entry is None:
            raise PluginStoreError(f"plugin is not installed: {name}")
        install_root = Path(entry.install_path).parent
        if install_root.exists():
            self._reject_links(install_root)
        data_root = Path(entry.data_path)
        if purge_data and data_root.exists():
            self._reject_links(data_root)
        self._write_entries(entries)
        self._remove_owned_tree(install_root, self.installs_dir)
        if purge_data:
            self._remove_owned_tree(data_root, self.data_dir)
        return entry

    def load_enabled(self) -> list[PluginBundle]:
        bundles: list[PluginBundle] = []
        for entry in self.list_installed():
            if not entry.enabled:
                continue
            try:
                bundles.append(self._load_locked(entry))
            except PluginStoreError as exc:
                bundles.append(self._broken_bundle(entry, str(exc)))
        return bundles

    def enabled_mcp_configs(self) -> list[MCPServerConfig]:
        """Return globally unique MCP declarations from intact enabled bundles."""
        configs: list[MCPServerConfig] = []
        for bundle in self.load_enabled():
            if bundle.manifest is None:
                continue
            configs.extend(
                replace(
                    server,
                    name=self._runtime_server_name(bundle.manifest.name, server.name),
                )
                for server in bundle.mcp_servers
            )
        return configs

    def _install_from_directory(
        self,
        source_path: Path,
        *,
        source_type: Literal["local", "git"],
        source: str,
        revision: str = "",
        expected_name: str | None = None,
        replacing: bool = False,
    ) -> PluginLockEntry:
        if not source_path.is_dir():
            raise PluginStoreError(f"plugin source is not a directory: {source_path}")
        if self.root.is_relative_to(source_path):
            raise PluginStoreError("plugin source must not contain the plugin store")
        self._reject_links(source_path)
        self.installs_dir.mkdir(parents=True, exist_ok=True)
        self.data_dir.mkdir(parents=True, exist_ok=True)
        staging = Path(tempfile.mkdtemp(prefix="install-", dir=self.installs_dir))
        try:
            shutil.copytree(
                source_path,
                staging,
                dirs_exist_ok=True,
                ignore=shutil.ignore_patterns(".git", ".gitmodules"),
            )
            preliminary = self.loader.load(
                staging, source_type=source_type, source=source, revision=revision
            )
            if preliminary.manifest is None or not preliminary.loadable:
                raise PluginStoreError(self._diagnostic_summary(preliminary.diagnostics))
            name = preliminary.manifest.name
            if expected_name is not None and name != expected_name:
                raise PluginStoreError(
                    f"update identity mismatch: expected {expected_name}, got {name}"
                )
            entries = self._read_entries()
            if name in entries and not replacing:
                raise PluginStoreError(f"plugin is already installed: {name}; use update")
            content_hash = self.loader.content_hash(staging)
            destination = (self.installs_dir / name / content_hash).resolve()
            if not destination.is_relative_to(self.installs_dir):
                raise PluginStoreError("resolved installation path escapes plugin store")
            destination.parent.mkdir(parents=True, exist_ok=True)
            if not destination.exists():
                os.replace(staging, destination)
            if self.loader.content_hash(destination) != content_hash:
                raise PluginStoreError(f"content-addressed installation is corrupted: {name}")
            bundle = self.loader.load(
                destination,
                source_type=source_type,
                source=source,
                revision=revision,
                data_root=(self.data_dir / name).resolve(),
            )
            data_path = (self.data_dir / name).resolve()
            data_path.mkdir(parents=True, exist_ok=True)
            entry = PluginLockEntry(
                name=name,
                version=bundle.manifest.version if bundle.manifest else "",
                enabled=False,
                source_type=source_type,
                source=source,
                revision=revision,
                content_hash=content_hash,
                install_path=str(destination),
                data_path=str(data_path),
                installed_at=datetime.now(UTC).isoformat(),
                diagnostics=[diagnostic.to_dict() for diagnostic in bundle.diagnostics],
                resolved_dependencies=self._runtime_dependencies(bundle),
                required_capabilities=self._required_capabilities(bundle),
            )
            entries[name] = entry
            self._write_entries(entries)
            return entry
        finally:
            if staging.exists():
                shutil.rmtree(staging)

    def _load_locked(self, entry: PluginLockEntry) -> PluginBundle:
        install_path = Path(entry.install_path).resolve()
        if not install_path.is_relative_to(self.installs_dir) or not install_path.is_dir():
            raise PluginStoreError(f"locked installation is missing or outside store: {entry.name}")
        data_path = Path(entry.data_path).resolve()
        if not data_path.is_relative_to(self.data_dir) or data_path == self.data_dir:
            raise PluginStoreError(f"locked data path is outside store: {entry.name}")
        actual_hash = self.loader.content_hash(install_path)
        if actual_hash != entry.content_hash:
            raise PluginStoreError(f"content hash mismatch for installed plugin: {entry.name}")
        bundle = self.loader.load(
            install_path,
            source_type=entry.source_type,
            source=entry.source,
            revision=entry.revision,
            data_root=data_path,
        )
        if bundle.manifest is None or bundle.manifest.name != entry.name:
            raise PluginStoreError(f"manifest identity mismatch for installed plugin: {entry.name}")
        return bundle

    def _read_entries(self) -> dict[str, PluginLockEntry]:
        if not self.lock_path.exists():
            return {}
        try:
            raw = json.loads(self.lock_path.read_text(encoding="utf-8"))
            if raw.get("lock_version") != 1 or not isinstance(raw.get("plugins"), dict):
                raise ValueError("unsupported lockfile shape")
            entries: dict[str, PluginLockEntry] = {}
            for name, value in raw["plugins"].items():
                if not isinstance(name, str) or not isinstance(value, dict):
                    raise ValueError("plugin entries must map names to objects")
                entry = PluginLockEntry.from_dict(value)
                self._validate_entry_types(entry)
                if entry.name != name or entry.source_type not in {"local", "git"}:
                    raise ValueError(f"invalid lock entry identity: {name}")
                if not re.fullmatch(r"[0-9a-f]{64}", entry.content_hash):
                    raise ValueError(f"invalid content hash: {name}")
                expected_install = (self.installs_dir / entry.name / entry.content_hash).resolve()
                expected_data = (self.data_dir / entry.name).resolve()
                if Path(entry.install_path).resolve() != expected_install:
                    raise ValueError(f"invalid installation path binding: {name}")
                if Path(entry.data_path).resolve() != expected_data:
                    raise ValueError(f"invalid data path binding: {name}")
                entries[name] = entry
            return entries
        except (OSError, ValueError, TypeError, json.JSONDecodeError) as exc:
            raise PluginStoreError(f"invalid plugin lockfile: {exc}") from exc

    def _write_entries(self, entries: dict[str, PluginLockEntry]) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        payload = {
            "lock_version": 1,
            "plugins": {name: asdict(entries[name]) for name in sorted(entries)},
        }
        descriptor, temporary_name = tempfile.mkstemp(prefix="plugins-lock-", dir=self.root)
        temporary = Path(temporary_name)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
                json.dump(payload, handle, indent=2, ensure_ascii=False, sort_keys=True)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self.lock_path)
        finally:
            temporary.unlink(missing_ok=True)

    @classmethod
    def _reject_links(cls, root: Path) -> None:
        if cls._is_link_like(root):
            raise PluginStoreError("plugin installation rejects symlink or reparse-point roots")
        for current, directories, files in os.walk(root, followlinks=False):
            for name in [*directories, *files]:
                path = Path(current) / name
                if cls._is_link_like(path):
                    raise PluginStoreError(
                        "plugin installation rejects symlinks and reparse points: "
                        f"{path.relative_to(root)}"
                    )

    @staticmethod
    def _is_link_like(path: Path) -> bool:
        if path.is_symlink():
            return True
        if os.name != "nt":
            return False
        try:
            attributes = getattr(path.lstat(), "st_file_attributes", 0)
        except OSError:
            return False
        reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
        return bool(attributes & reparse_flag)

    @staticmethod
    def _runtime_server_name(plugin_name: str, server_name: str) -> str:
        """Create a provider-safe, collision-resistant MCP namespace."""

        def component(value: str) -> str:
            slug = re.sub(r"[^A-Za-z0-9_-]+", "_", value).strip("_")[:12] or "component"
            digest = hashlib.sha256(value.encode("utf-8")).hexdigest()[:10]
            return f"{slug}_{digest}"

        return f"plugin_{component(plugin_name)}_{component(server_name)}"

    @staticmethod
    def _runtime_dependencies(bundle: PluginBundle) -> list[str]:
        dependencies: set[str] = {
            server.command
            for server in bundle.mcp_servers
            if PluginStore._is_bare_command(server.command)
        }
        dependencies.update(
            command
            for hook in bundle.hooks
            if hook.handler_type == "command"
            and isinstance((command := hook.definition.get("command")), str)
            and PluginStore._is_bare_command(command)
        )
        return sorted(dependencies)

    @staticmethod
    def _is_bare_command(command: str) -> bool:
        return (
            bool(command)
            and not Path(command).is_absolute()
            and "/" not in command
            and "\\" not in command
        )

    @staticmethod
    def _required_capabilities(bundle: PluginBundle) -> list[str]:
        capabilities = {capability for hook in bundle.hooks for capability in hook.capabilities}
        capabilities.update(
            capability for server in bundle.mcp_servers for capability in server.capabilities
        )
        for server in bundle.mcp_servers:
            if server.transport == MCPTransport.STDIO:
                capabilities.add("execute")
            elif server.transport == MCPTransport.HTTP:
                capabilities.add("network")
        return sorted(capabilities)

    @staticmethod
    def _validate_entry_types(entry: PluginLockEntry) -> None:
        string_values = (
            entry.name,
            entry.version,
            entry.source,
            entry.revision,
            entry.content_hash,
            entry.install_path,
            entry.data_path,
            entry.installed_at,
        )
        if any(not isinstance(value, str) for value in string_values):
            raise ValueError(f"lock entry contains a non-string field: {entry.name!r}")
        if type(entry.enabled) is not bool:
            raise ValueError(f"enabled must be a boolean: {entry.name}")
        if entry.source_type not in {"local", "git"}:
            raise ValueError(f"invalid source type: {entry.name}")
        if entry.source_type == "git" and not _COMMIT_SHA.fullmatch(entry.revision):
            raise ValueError(f"invalid Git revision: {entry.name}")
        if not isinstance(entry.diagnostics, list) or any(
            not isinstance(item, dict)
            or any(
                not isinstance(key, str) or not isinstance(value, str)
                for key, value in item.items()
            )
            for item in entry.diagnostics
        ):
            raise ValueError(f"invalid diagnostics: {entry.name}")
        for field_name, values in (
            ("resolved_dependencies", entry.resolved_dependencies),
            ("required_capabilities", entry.required_capabilities),
        ):
            if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
                raise ValueError(f"invalid {field_name}: {entry.name}")

    @staticmethod
    def _diagnostic_summary(diagnostics: list[PluginDiagnostic]) -> str:
        blockers = [item.message for item in diagnostics if item.level == "blocker"]
        return "; ".join(blockers) or "plugin manifest is not loadable"

    def _broken_bundle(self, entry: PluginLockEntry, message: str) -> PluginBundle:
        from app.plugins.models import PluginProvenance

        return PluginBundle(
            root=Path(entry.install_path),
            manifest=None,
            provenance=PluginProvenance(
                source_type=entry.source_type,
                source=entry.source,
                revision=entry.revision,
                content_hash=entry.content_hash,
            ),
            diagnostics=[
                PluginDiagnostic(
                    code="store.integrity_error",
                    message=message,
                    level="blocker",
                    status="unsafe",
                    path=entry.install_path,
                )
            ],
        )

    @staticmethod
    def _validate_git_source(source: str) -> None:
        if not source or source.startswith("-") or "\x00" in source:
            raise PluginStoreError("Git source is empty or looks like a command option")

    @staticmethod
    def _git(*arguments: str) -> str:
        try:
            result = subprocess.run(
                ["git", *arguments],
                capture_output=True,
                text=True,
                check=True,
                timeout=120,
                shell=False,
            )
        except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
            detail = (
                exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else str(exc)
            )
            raise PluginStoreError(f"Git source operation failed: {detail}") from exc
        return result.stdout

    @staticmethod
    def _remove_owned_tree(path: Path, owner: Path) -> None:
        resolved, resolved_owner = path.resolve(), owner.resolve()
        if not resolved.is_relative_to(resolved_owner) or resolved == resolved_owner:
            raise PluginStoreError(f"refusing to remove path outside owned root: {resolved}")
        if not resolved.exists():
            return
        for child in resolved.rglob("*"):
            with suppress(OSError):
                child.chmod(stat.S_IWRITE | stat.S_IREAD)
        shutil.rmtree(resolved)
