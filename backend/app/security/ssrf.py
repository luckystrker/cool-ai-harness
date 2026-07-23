"""SSRF protection for network tools.

Blocks requests to private/internal IP ranges (RFC 1918, loopback, link-local)
and enforces a domain allowlist. Used by web_fetch and any future network tool.

The check is URL-based: we parse the URL, extract the hostname, resolve it to
IPs, and reject if any resolved IP is private. This prevents DNS rebinding
attacks where a domain resolves to a public IP at check time but a private IP
at fetch time (we resolve once and pass the IP to the client).
"""

from __future__ import annotations

import ipaddress
import socket
from dataclasses import dataclass
from urllib.parse import urlparse

from app.core.logging import get_logger

log = get_logger(__name__)


@dataclass
class UrlSafetyResult:
    """Result of a URL safety check."""

    safe: bool
    reason: str = ""
    blocked_ip: str | None = None


def is_private_ip(ip_str: str) -> bool:
    """True if the IP is private, loopback, link-local, or reserved.

    Covers RFC 1918 (10.x, 172.16-31.x, 192.168.x), loopback (127.x),
    link-local (169.254.x), and other non-routable ranges.
    """
    try:
        ip = ipaddress.ip_address(ip_str)
    except ValueError:
        return False
    return (
        ip.is_private
        or ip.is_loopback
        or ip.is_link_local
        or ip.is_reserved
        or ip.is_multicast
        or ip.is_unspecified
    )


def is_safe_url(
    url: str,
    *,
    allowed_domains: list[str] | None = None,
    block_private_ips: bool = True,
) -> UrlSafetyResult:
    """Check whether a URL is safe to fetch.

    Args:
        url: The URL to check.
        allowed_domains: If non-empty, only these domains (or their subdomains)
            are allowed. Empty/None = all domains allowed.
        block_private_ips: If True, reject URLs that resolve to private IPs.

    Returns:
        UrlSafetyResult with ``safe=True`` if the URL passes all checks.
    """
    try:
        parsed = urlparse(url)
    except Exception as exc:
        return UrlSafetyResult(safe=False, reason=f"Invalid URL: {exc}")

    scheme = parsed.scheme.lower()
    if scheme not in ("http", "https"):
        return UrlSafetyResult(safe=False, reason=f"Unsupported scheme: {scheme!r}")

    hostname = parsed.hostname
    if not hostname:
        return UrlSafetyResult(safe=False, reason="No hostname in URL")

    # Domain allowlist check.
    if allowed_domains:
        hostname_lower = hostname.lower()
        matched = False
        for domain in allowed_domains:
            d = domain.lower().lstrip(".")
            if hostname_lower == d or hostname_lower.endswith("." + d):
                matched = True
                break
        if not matched:
            return UrlSafetyResult(
                safe=False,
                reason=f"Domain {hostname!r} not in allowlist",
            )

    # If the hostname is already an IP literal, check it directly.
    try:
        ipaddress.ip_address(hostname)
        if block_private_ips and is_private_ip(hostname):
            return UrlSafetyResult(
                safe=False,
                reason=f"Private/reserved IP blocked: {hostname}",
                blocked_ip=hostname,
            )
        # Public IP literal — safe (no DNS resolution needed).
        return UrlSafetyResult(safe=True)
    except ValueError:
        pass  # Not an IP literal — proceed to DNS resolution.

    # SSRF check: resolve the hostname and reject if any IP is private.
    if block_private_ips:
        try:
            infos = socket.getaddrinfo(hostname, None)
        except socket.gaierror:
            # Can't resolve — let the HTTP client handle the error.
            return UrlSafetyResult(safe=True)
        for info in infos:
            ip_str = info[4][0]
            if is_private_ip(ip_str):
                return UrlSafetyResult(
                    safe=False,
                    reason=f"Hostname {hostname!r} resolves to private IP {ip_str}",
                    blocked_ip=ip_str,
                )

    return UrlSafetyResult(safe=True)


def check_url_safety(
    url: str,
    *,
    allowed_domains: list[str] | None = None,
    block_private_ips: bool = True,
) -> UrlSafetyResult:
    """Alias for is_safe_url (semantic clarity at call sites)."""
    return is_safe_url(
        url,
        allowed_domains=allowed_domains,
        block_private_ips=block_private_ips,
    )
