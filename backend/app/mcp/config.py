"""MCP configuration loading from config.yaml (Фаза 2 §4).

Supports a ``config.yaml`` file at the repository root (or a path set via
``MCP_CONFIG_FILE`` env var) with an ``mcp_servers`` key::

    mcp_servers:
      - name: filesystem
        transport: stdio
        command: npx
        args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        enabled: true

      - name: remote-tools
        transport: http
        url: http://localhost:8080/mcp
        headers:
          Authorization: "Bearer ${MCP_TOKEN}"

Environment variable interpolation (``${VAR}``) is supported in string values.
"""

from __future__ import annotations

import os
import re
from pathlib import Path
from typing import Any

from app.core.config import get_settings
from app.core.logging import get_logger
from app.mcp.models import MCPServerConfig

log = get_logger(__name__)

# Pattern for ${ENV_VAR} interpolation.
_ENV_RE = re.compile(r"\$\{([^}]+)\}")


def _interpolate_env(value: str) -> str:
    """Replace ${VAR} placeholders with environment variable values."""

    def _replace(m: re.Match[str]) -> str:
        var_name = m.group(1)
        return os.environ.get(var_name, "")

    return _ENV_RE.sub(_replace, value)


def _interpolate_recursive(obj: Any) -> Any:
    """Recursively interpolate env vars in all string values."""
    if isinstance(obj, str):
        return _interpolate_env(obj)
    if isinstance(obj, dict):
        return {k: _interpolate_recursive(v) for k, v in obj.items()}
    if isinstance(obj, list):
        return [_interpolate_recursive(item) for item in obj]
    return obj


def get_config_path() -> Path:
    """Determine the config.yaml path."""
    # Preserve the pre-M2 override while making COOL_CONFIG_FILE / COOL_HOME
    # the shared package contract for all mutable runtime paths.
    env_path = os.environ.get("MCP_CONFIG_FILE", "")
    if env_path:
        return Path(env_path)
    return get_settings().cool_config_file


def load_mcp_configs() -> list[MCPServerConfig]:
    """Load MCP server configurations from config.yaml.

    Returns an empty list if the file doesn't exist or has no mcp_servers key.
    Uses PyYAML if available; falls back to a minimal parser for simple cases.
    """
    config_path = get_config_path()
    if not config_path.exists():
        log.debug("mcp.config_not_found", path=str(config_path))
        return []

    try:
        data = _read_yaml(config_path)
    except Exception as exc:
        log.error("mcp.config_parse_error", path=str(config_path), error=str(exc))
        return []

    if not isinstance(data, dict):
        return []

    servers_raw = data.get("mcp_servers", [])
    if not isinstance(servers_raw, list):
        log.warning("mcp.config_invalid_servers", path=str(config_path))
        return []

    # Interpolate environment variables.
    servers_raw = _interpolate_recursive(servers_raw)

    configs: list[MCPServerConfig] = []
    for entry in servers_raw:
        if not isinstance(entry, dict) or "name" not in entry:
            log.warning("mcp.config_skipping_entry", entry=str(entry)[:100])
            continue
        try:
            configs.append(MCPServerConfig.from_dict(entry))
        except (KeyError, ValueError) as exc:
            log.warning("mcp.config_invalid_entry", name=entry.get("name", "?"), error=str(exc))

    log.info("mcp.configs_from_yaml", count=len(configs), path=str(config_path))
    return configs


def save_mcp_configs(configs: list[MCPServerConfig], *, backup: bool = True) -> None:
    """Persist MCP server configs to config.yaml (merges with existing content).

    Only updates the ``mcp_servers`` key; other keys are preserved.
    When ``backup=True``, saves the previous config to ``config.yaml.bak``
    for safe rollback (Фаза 2 §2 lifecycle).
    """
    config_path = get_config_path()

    existing: dict[str, Any] = {}
    if config_path.exists():
        try:
            existing = _read_yaml(config_path) or {}
        except Exception:
            existing = {}

    # Save backup for rollback before overwriting.
    if backup and config_path.exists():
        backup_path = config_path.with_suffix(".yaml.bak")
        try:
            backup_path.write_text(config_path.read_text(encoding="utf-8"), encoding="utf-8")
            log.debug("mcp.config_backup_saved", path=str(backup_path))
        except Exception as exc:
            log.warning("mcp.config_backup_failed", error=str(exc))

    existing["mcp_servers"] = [c.to_dict() for c in configs]

    try:
        _write_yaml(config_path, existing)
        log.info("mcp.configs_saved", count=len(configs), path=str(config_path))
    except Exception as exc:
        log.error("mcp.config_save_error", path=str(config_path), error=str(exc))


def rollback_mcp_configs() -> list[MCPServerConfig] | None:
    """Rollback to the previous config.yaml backup (Фаза 2 §2 safe rollback).

    Restores ``config.yaml.bak`` over ``config.yaml`` and returns the
    restored configs. Returns None if no backup exists.
    """
    config_path = get_config_path()
    backup_path = config_path.with_suffix(".yaml.bak")

    if not backup_path.exists():
        log.warning("mcp.rollback_no_backup")
        return None

    try:
        config_path.write_text(backup_path.read_text(encoding="utf-8"), encoding="utf-8")
        log.info("mcp.config_rolled_back", path=str(config_path))
    except Exception as exc:
        log.error("mcp.rollback_failed", error=str(exc))
        return None

    return load_mcp_configs()


def _read_yaml(path: Path) -> dict[str, Any]:
    """Read a YAML file. Uses PyYAML if available, else a JSON fallback."""
    text = path.read_text(encoding="utf-8")
    try:
        import yaml

        return yaml.safe_load(text) or {}
    except ImportError:
        pass
    # Fallback: try JSON (config.yaml might actually be JSON).
    import json

    try:
        return json.loads(text)
    except json.JSONDecodeError:
        raise ValueError("PyYAML not installed and config.yaml is not valid JSON") from None


def _write_yaml(path: Path, data: dict[str, Any]) -> None:
    """Write data to a YAML file."""
    try:
        import yaml

        text = yaml.dump(data, default_flow_style=False, sort_keys=False, allow_unicode=True)
    except ImportError:
        import json

        text = json.dumps(data, indent=2, ensure_ascii=False)
    path.write_text(text, encoding="utf-8")
