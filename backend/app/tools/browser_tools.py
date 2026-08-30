"""Stateful Playwright browser automation tools with SSRF confinement."""

from __future__ import annotations

import asyncio
import ipaddress
import socket
import time
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlparse

import aiohttp
from aiohttp.abc import AbstractResolver, ResolveResult
from pydantic import Field
from sqlmodel import Session

from app.artifacts import store_artifact
from app.core.config import get_settings
from app.core.db import engine
from app.security.capabilities import Capability
from app.security.ssrf import check_url_safety
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context


@dataclass
class _BrowserSession:
    playwright: Any
    browser: Any
    context: Any
    page: Any
    touched_at: float
    hostname: str
    pinned_ip: str
    network_bytes: int = 0
    http: Any = None


class _PinnedResolver(AbstractResolver):
    """aiohttp resolver that never performs a second DNS lookup."""

    def __init__(self, hostname: str, pinned_ip: str) -> None:
        self.hostname = hostname
        self.pinned_ip = pinned_ip

    async def resolve(
        self,
        host: str,
        port: int = 0,
        family: socket.AddressFamily = socket.AF_INET,
    ) -> list[ResolveResult]:
        if host.lower() != self.hostname:
            raise OSError("Cross-origin browser request blocked")
        return [
            {
                "hostname": host,
                "host": self.pinned_ip,
                "port": port,
                "family": family,
                "proto": 0,
                "flags": 0,
            }
        ]

    async def close(self) -> None:
        return None


class BrowserSessionManager:
    """One isolated browser context per durable run/conversation."""

    def __init__(self, max_sessions: int = 8) -> None:
        self._sessions: dict[str, _BrowserSession] = {}
        self._lock = asyncio.Lock()
        self.max_sessions = max_sessions

    @staticmethod
    def key() -> str:
        ctx = get_run_context()
        return (
            f"run:{ctx.run_id}" if ctx.run_id is not None else f"conversation:{ctx.conversation_id}"
        )

    async def get(self, url: str | None = None) -> _BrowserSession:
        key = self.key()
        parsed_hostname = urlparse(url).hostname if url else None
        hostname = parsed_hostname.lower() if parsed_hostname else None
        async with self._lock:
            existing = self._sessions.get(key)
            if existing is not None and (hostname is None or existing.hostname == hostname):
                existing.touched_at = time.monotonic()
                return existing
            if existing is not None:
                await self._close_locked(key)
            if url is None or hostname is None:
                raise RuntimeError("Navigate to a URL before using browser tools")
            safety = _url_safety(url)
            if not safety.safe or not safety.resolved_ips:
                raise RuntimeError(f"URL blocked (SSRF protection): {safety.reason}")
            pinned_ip = next(
                (
                    ip
                    for ip in safety.resolved_ips
                    if ipaddress.ip_address(ip).version == 4
                ),
                safety.resolved_ips[0],
            )
            if len(self._sessions) >= self.max_sessions:
                oldest_key = min(self._sessions, key=lambda item: self._sessions[item].touched_at)
                await self._close_locked(oldest_key)
            try:
                from playwright.async_api import async_playwright
            except ImportError as exc:
                raise RuntimeError("Playwright is not installed") from exc
            playwright = await async_playwright().start()
            try:
                browser = await playwright.chromium.launch(
                    headless=get_settings().browser_headless,
                    args=[f"--host-resolver-rules=MAP {hostname} {pinned_ip},EXCLUDE localhost"],
                )
                context = await browser.new_context(
                    user_agent=(
                        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                        "AppleWebKit/537.36 (KHTML, like Gecko) "
                        "Chrome/131.0 Safari/537.36"
                    ),
                    viewport={"width": 1440, "height": 1000},
                    accept_downloads=False,
                    service_workers="block",
                )
                # Small, explicit stealth shim. It avoids the most basic bot
                # branch without pretending to bypass access controls.
                await context.add_init_script(
                    "Object.defineProperty(navigator, 'webdriver', {get: () => undefined});"
                )
                session = _BrowserSession(
                    playwright=playwright,
                    browser=browser,
                    context=context,
                    page=None,
                    touched_at=time.monotonic(),
                    hostname=hostname,
                    pinned_ip=pinned_ip,
                )
                connector = aiohttp.TCPConnector(
                    resolver=_PinnedResolver(hostname, pinned_ip),
                    use_dns_cache=True,
                    limit=8,
                )
                session.http = aiohttp.ClientSession(
                    connector=connector,
                    auto_decompress=False,
                    trust_env=False,
                )
                await context.route(
                    "**/*",
                    lambda route, request: _route_with_ssrf_guard(route, request, session),
                )
                if hasattr(context, "route_web_socket"):
                    await context.route_web_socket("**/*", _block_websocket)
                page = await context.new_page()
                session.page = page
            except Exception:
                await playwright.stop()
                raise
            self._sessions[key] = session
            return session

    async def close(self, key: str | None = None) -> bool:
        async with self._lock:
            resolved = key or self.key()
            if resolved not in self._sessions:
                return False
            await self._close_locked(resolved)
            return True

    async def close_all(self) -> None:
        async with self._lock:
            for key in list(self._sessions):
                await self._close_locked(key)

    async def _close_locked(self, key: str) -> None:
        session = self._sessions.pop(key, None)
        if session is None:
            return
        try:
            if session.http is not None:
                await session.http.close()
            await session.context.close()
            await session.browser.close()
        finally:
            await session.playwright.stop()


browser_sessions = BrowserSessionManager()


async def _block_websocket(websocket: Any) -> None:
    await websocket.close(code=1008, reason="WebSockets are disabled in browser tools")


async def _route_with_ssrf_guard(
    route: Any, request: Any, session: _BrowserSession
) -> None:
    parsed = urlparse(request.url)
    if parsed.scheme in {"about", "data", "blob"}:
        await route.continue_()
        return
    request_hostname = (parsed.hostname or "").lower()
    if parsed.scheme not in {"http", "https"} or request_hostname != session.hostname:
        await route.abort("blockedbyclient")
        return
    settings = get_settings()
    try:
        headers = dict(request.headers)
        for header in ("host", "content-length", "connection"):
            headers.pop(header, None)
        timeout = aiohttp.ClientTimeout(total=settings.browser_timeout_ms / 1000)
        async with session.http.request(
            request.method,
            request.url,
            headers=headers,
            data=request.post_data_buffer,
            allow_redirects=False,
            timeout=timeout,
        ) as response:
            declared_size = int(response.headers.get("content-length", "0") or 0)
            if declared_size > settings.browser_max_resource_bytes:
                await route.abort("blockedbyclient")
                return
            body = bytearray()
            async for chunk in response.content.iter_chunked(64 * 1024):
                if len(body) + len(chunk) > settings.browser_max_resource_bytes:
                    await route.abort("blockedbyclient")
                    return
                if (
                    session.network_bytes + len(body) + len(chunk)
                    > settings.browser_max_session_bytes
                ):
                    await route.abort("blockedbyclient")
                    return
                body.extend(chunk)
            session.network_bytes += len(body)
            await route.fulfill(
                status=response.status,
                headers=dict(response.headers),
                body=bytes(body),
            )
    except Exception:
        await route.abort("failed")


def _url_safety(url: str):
    settings = get_settings()
    return check_url_safety(
        url,
        allowed_domains=settings.network_allowed_domains or None,
        block_private_ips=settings.ssrf_block_private_ips,
    )


class BrowserNavigateArgs(ToolArgs):
    url: str
    wait_until: str = Field(
        default="domcontentloaded", pattern="^(commit|domcontentloaded|load|networkidle)$"
    )


class BrowserClickArgs(ToolArgs):
    selector: str
    wait_for_navigation: bool = False


class BrowserFillArgs(ToolArgs):
    selector: str
    value: str


class BrowserScrollArgs(ToolArgs):
    selector: str | None = Field(
        default=None, description="Optional CSS/XPath selector to scroll into view"
    )
    y: int = Field(default=800, ge=-10_000, le=10_000)


class BrowserExtractArgs(ToolArgs):
    selector: str = "body"
    attribute: str | None = None
    all_matches: bool = False
    max_chars: int = Field(default=20_000, ge=1, le=100_000)


class BrowserScreenshotArgs(ToolArgs):
    full_page: bool = True
    filename: str = "browser-screenshot.png"


class BrowserCloseArgs(ToolArgs):
    pass


async def browser_navigate(*, url: str, wait_until: str = "domcontentloaded") -> ToolResult:
    safety = _url_safety(url)
    if not safety.safe:
        return ToolResult.err(f"URL blocked (SSRF protection): {safety.reason}")
    try:
        session = await browser_sessions.get(url)
        response = await session.page.goto(
            url,
            wait_until=wait_until,
            timeout=get_settings().browser_timeout_ms,
        )
        final_hostname = urlparse(session.page.url).hostname
        if final_hostname is not None and final_hostname.lower() != session.hostname:
            await session.page.goto("about:blank")
            return ToolResult.err("Navigation redirected to a blocked URL")
        title = await session.page.title()
        text = (await session.page.locator("body").inner_text())[:4_000]
        return ToolResult.ok(
            f"URL: {session.page.url}\nTitle: {title}\n\n{text}",
            url=session.page.url,
            title=title,
            status=response.status if response else None,
        )
    except Exception as exc:
        return ToolResult.err(_browser_error(exc))


async def browser_click(*, selector: str, wait_for_navigation: bool = False) -> ToolResult:
    try:
        session = await browser_sessions.get()
        locator = session.page.locator(selector).first
        if wait_for_navigation:
            async with session.page.expect_navigation(
                wait_until="domcontentloaded", timeout=get_settings().browser_timeout_ms
            ):
                await locator.click(timeout=get_settings().browser_timeout_ms)
        else:
            await locator.click(timeout=get_settings().browser_timeout_ms)
        return ToolResult.ok(
            f"Clicked {selector}\nCurrent URL: {session.page.url}", url=session.page.url
        )
    except Exception as exc:
        return ToolResult.err(_browser_error(exc))


async def browser_fill(*, selector: str, value: str) -> ToolResult:
    try:
        session = await browser_sessions.get()
        await session.page.locator(selector).first.fill(
            value, timeout=get_settings().browser_timeout_ms
        )
        return ToolResult.ok(f"Filled {selector}")
    except Exception as exc:
        return ToolResult.err(_browser_error(exc))


async def browser_scroll(*, selector: str | None = None, y: int = 800) -> ToolResult:
    try:
        session = await browser_sessions.get()
        if selector:
            await session.page.locator(selector).first.scroll_into_view_if_needed(
                timeout=get_settings().browser_timeout_ms
            )
        else:
            await session.page.evaluate("delta => window.scrollBy(0, delta)", y)
        position = await session.page.evaluate("() => window.scrollY")
        return ToolResult.ok(f"Scrolled to y={position}", y=position, url=session.page.url)
    except Exception as exc:
        return ToolResult.err(_browser_error(exc))


async def browser_extract(
    *,
    selector: str = "body",
    attribute: str | None = None,
    all_matches: bool = False,
    max_chars: int = 20_000,
) -> ToolResult:
    try:
        session = await browser_sessions.get()
        locator = session.page.locator(selector)
        count = await locator.count()
        targets = [locator.nth(index) for index in range(count if all_matches else min(count, 1))]
        values: list[str] = []
        for target in targets:
            value = (
                await target.get_attribute(attribute) if attribute else await target.inner_text()
            )
            values.append(value or "")
        output = "\n\n".join(values)
        return ToolResult.ok(
            output[:max_chars],
            selector=selector,
            matches=len(values),
            truncated=len(output) > max_chars,
            url=session.page.url,
        )
    except Exception as exc:
        return ToolResult.err(_browser_error(exc))


async def browser_screenshot(
    *, full_page: bool = True, filename: str = "browser-screenshot.png"
) -> ToolResult:
    try:
        session = await browser_sessions.get()
        content = await session.page.screenshot(full_page=full_page, type="png")
        ctx = get_run_context()
        if ctx.conversation_id is None:
            return ToolResult.ok(
                "Screenshot captured but no conversation is available for artifact storage",
                size_bytes=len(content),
            )
        safe_filename = filename if filename.lower().endswith(".png") else f"{filename}.png"
        with Session(engine) as db:
            artifact = store_artifact(
                db,
                conversation_id=ctx.conversation_id,
                run_id=ctx.run_id,
                filename=safe_filename,
                content=content,
                kind="image",
                media_type="image/png",
                metadata={"source_url": session.page.url, "browser_screenshot": True},
            )
        return ToolResult.ok(
            f"Screenshot saved as artifact {artifact.id}",
            artifact_id=artifact.id,
            screenshot_url=(
                f"/api/conversations/{ctx.conversation_id}/artifacts/{artifact.id}/download"
            ),
            url=session.page.url,
        )
    except Exception as exc:
        return ToolResult.err(_browser_error(exc))


async def browser_close() -> ToolResult:
    closed = await browser_sessions.close()
    return ToolResult.ok("Browser session closed" if closed else "No browser session was open")


def _browser_error(exc: Exception) -> str:
    message = str(exc)
    if "Executable doesn't exist" in message:
        return "Chromium is not installed. Run: python -m playwright install chromium"
    return f"Browser automation failed: {message}"


def register_browser_tools() -> None:
    network = frozenset({Capability.NETWORK})
    interactive = frozenset({Capability.NETWORK, Capability.SEND_EXTERNAL})
    register_tool(
        name="browser_navigate",
        description="Open a URL in an isolated headless browser session and return visible text.",
        args_model=BrowserNavigateArgs,
        func=browser_navigate,
        capabilities=network,
    )
    register_tool(
        name="browser_click",
        description="Click the first element matching a CSS selector in the current browser page.",
        args_model=BrowserClickArgs,
        func=browser_click,
        dangerous=True,
        capabilities=interactive,
    )
    register_tool(
        name="browser_fill",
        description="Fill an input matching a CSS selector. Never use it to submit secrets.",
        args_model=BrowserFillArgs,
        func=browser_fill,
        dangerous=True,
        capabilities=interactive,
    )
    register_tool(
        name="browser_extract",
        description="Extract text or an attribute from CSS-selected elements on the current page.",
        args_model=BrowserExtractArgs,
        func=browser_extract,
        capabilities=network,
    )
    register_tool(
        name="browser_scroll",
        description="Scroll the page or bring a CSS/XPath-selected element into view.",
        args_model=BrowserScrollArgs,
        func=browser_scroll,
        capabilities=network,
    )
    register_tool(
        name="browser_screenshot",
        description="Capture the current browser page and save it as a conversation artifact.",
        args_model=BrowserScreenshotArgs,
        func=browser_screenshot,
        capabilities=frozenset({Capability.NETWORK, Capability.WRITE}),
    )
    register_tool(
        name="browser_close",
        description="Close the current isolated browser session and release its resources.",
        args_model=BrowserCloseArgs,
        func=browser_close,
        capabilities=network,
    )
