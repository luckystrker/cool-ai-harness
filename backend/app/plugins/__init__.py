"""Canonical Agent Plugins contract and lifecycle support."""

from app.plugins.loader import PluginLoader
from app.plugins.models import PluginBundle, PluginDiagnostic, PluginManifest

__all__ = ["PluginBundle", "PluginDiagnostic", "PluginLoader", "PluginManifest"]
