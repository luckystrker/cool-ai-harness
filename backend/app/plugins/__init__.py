"""Canonical Agent Plugins contract and lifecycle support."""

from app.plugins.compatibility import CompatibilityLoader
from app.plugins.loader import PluginLoader
from app.plugins.models import PluginBundle, PluginDiagnostic, PluginManifest
from app.plugins.store import PluginLockEntry, PluginStore, PluginStoreError

__all__ = [
    "CompatibilityLoader",
    "PluginBundle",
    "PluginDiagnostic",
    "PluginLoader",
    "PluginLockEntry",
    "PluginManifest",
    "PluginStore",
    "PluginStoreError",
]
