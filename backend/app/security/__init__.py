"""Capability security layer (Фаза 1.5 §2).

Coarse-grained permission gates that sit *above* the per-tool permission
system. Each tool declares the capabilities it needs (read, write, execute,
network, git, send_external). A capability policy maps each capability to
allow|ask|deny. The executor resolves both layers: if any required capability
is denied, the tool is denied; if any is "ask", the tool falls to "ask".

This package also provides SSRF protection, secret masking, sandbox env
preparation, and HITL breakpoint resolution.
"""

from __future__ import annotations

from app.security.breakpoints import (
    BreakpointConfig,
    BreakpointsConfig,
    BreakpointType,
)
from app.security.capabilities import (
    Capability,
    CapabilityPolicy,
    tool_capabilities,
)
from app.security.sandbox import build_sandbox_env
from app.security.secrets import mask_secrets
from app.security.ssrf import check_url_safety, is_safe_url

__all__ = [
    "BreakpointConfig",
    "BreakpointType",
    "BreakpointsConfig",
    "Capability",
    "CapabilityPolicy",
    "build_sandbox_env",
    "check_url_safety",
    "is_safe_url",
    "mask_secrets",
    "tool_capabilities",
]
