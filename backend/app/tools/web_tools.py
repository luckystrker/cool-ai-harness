"""Web tools: search and fetch.

``web_search`` is provider-pluggable: configure via the SEARCH_PROVIDER env
var (serper | tavily | searxng). Falls back to a "no provider configured"
error with a hint when unset, so the tool degrades gracefully offline.

``web_fetch`` pulls a URL with httpx and extracts main-text content. For now
extraction is plain HTML→text via stdlib + a heuristic <script>/<style>
strip; trafilatura will be added as an optional dependency in Фаза 4.
"""

from __future__ import annotations

import re
from typing import Any

import httpx

from app.core.config import get_settings
from app.security.capabilities import Capability
from app.security.secrets import mask_tool_output
from app.security.ssrf import check_url_safety
from app.tools.base import ToolArgs, ToolResult, register_tool

# --- web_search -----------------------------------------------------------

class WebSearchArgs(ToolArgs):
    query: str
    num_results: int = 5


async def web_search(*, query: str, num_results: int = 5) -> ToolResult:
    """Search the web. Provider chosen by SEARCH_* settings."""
    settings = get_settings()
    provider = (settings.search_provider or "").lower()

    if provider == "serper":
        return await _serper_search(query, num_results, settings)
    if provider == "tavily":
        return await _tavily_search(query, num_results, settings)
    if provider == "searxng":
        return await _searxng_search(query, num_results, settings)

    return ToolResult.err(
        "No web search provider configured. Set SEARCH_PROVIDER=serper|tavily|searxng "
        "and the matching API key / URL in your .env.",
        provider=None,
    )


async def _serper_search(query: str, num_results: int, settings: Any) -> ToolResult:
    if not settings.serper_api_key:
        return ToolResult.err("SEARCH_PROVIDER=serper but SERPER_API_KEY is empty.")
    async with httpx.AsyncClient(timeout=20) as client:
        resp = await client.post(
            "https://google.serper.dev/search",
            headers={"X-API-KEY": settings.serper_api_key},
            json={"q": query, "num": num_results},
        )
        resp.raise_for_status()
        data = resp.json()
    organic = data.get("organic", [])[:num_results]
    return _format_results(query, organic, key_title="title", key_link="link", key_snippet="snippet")


async def _tavily_search(query: str, num_results: int, settings: Any) -> ToolResult:
    if not settings.tavily_api_key:
        return ToolResult.err("SEARCH_PROVIDER=tavily but TAVILY_API_KEY is empty.")
    async with httpx.AsyncClient(timeout=20) as client:
        resp = await client.post(
            "https://api.tavily.com/search",
            json={
                "api_key": settings.tavily_api_key,
                "query": query,
                "max_results": num_results,
            },
        )
        resp.raise_for_status()
        data = resp.json()
    results = data.get("results", [])[:num_results]
    return _format_results(query, results, key_title="title", key_link="url", key_snippet="content")


async def _searxng_search(query: str, num_results: int, settings: Any) -> ToolResult:
    url = settings.searxng_url
    if not url:
        return ToolResult.err("SEARCH_PROVIDER=searxng but SEARXNG_URL is empty.")
    async with httpx.AsyncClient(timeout=20) as client:
        resp = await client.get(
            url.rstrip("/") + "/search",
            params={"q": query, "format": "json"},
        )
        resp.raise_for_status()
        data = resp.json()
    results = data.get("results", [])[:num_results]
    return _format_results(query, results, key_title="title", key_link="url", key_snippet="content")


def _format_results(
    query: str,
    items: list[dict[str, Any]],
    *,
    key_title: str,
    key_link: str,
    key_snippet: str,
) -> ToolResult:
    if not items:
        return ToolResult.ok(f"No results for: {query}", count=0)
    lines = [f"# Web search: {query}", f"{len(items)} result(s)\n"]
    for i, item in enumerate(items, 1):
        title = item.get(key_title) or "(untitled)"
        link = item.get(key_link) or ""
        snippet = item.get(key_snippet) or ""
        lines.append(f"## {i}. {title}\nURL: {link}\n{snippet}\n")
    return ToolResult.ok("\n".join(lines), count=len(items))


# --- web_fetch ------------------------------------------------------------

class WebFetchArgs(ToolArgs):
    url: str
    max_chars: int = 20_000


_TAG_SCRIPT = re.compile(rb"<(script|style)\b.*?</\1>", re.IGNORECASE | re.DOTALL)
_TAG = re.compile(rb"<[^>]+>")
_WS = re.compile(rb"\s+")


async def web_fetch(*, url: str, max_chars: int = 20_000) -> ToolResult:
    """Fetch a URL and return extracted main text (HTML stripped).

    SSRF-protected: blocks private IPs, enforces domain allowlist, and caps
    response body size.
    """
    settings = get_settings()

    # SSRF check: block private IPs and enforce domain allowlist.
    safety = check_url_safety(
        url,
        allowed_domains=settings.network_allowed_domains or None,
        block_private_ips=settings.ssrf_block_private_ips,
    )
    if not safety.safe:
        return ToolResult.err(
            f"URL blocked (SSRF protection): {safety.reason}",
            blocked_url=url,
            blocked_ip=safety.blocked_ip,
        )

    max_bytes = settings.network_max_response_bytes
    async with httpx.AsyncClient(timeout=30, follow_redirects=True) as client:
        # Stream the response so we can enforce a size limit without downloading
        # an arbitrarily large body.
        async with client.stream(
            "GET", url, headers={"User-Agent": "CoolAIHarness/0.1"}
        ) as resp:
            resp.raise_for_status()
            chunks: list[bytes] = []
            total = 0
            size_truncated = False
            async for chunk in resp.aiter_bytes(chunk_size=8192):
                total += len(chunk)
                if max_bytes and total > max_bytes:
                    size_truncated = True
                    # Keep only up to the limit.
                    remaining = max_bytes - (total - len(chunk))
                    if remaining > 0:
                        chunks.append(chunk[:remaining])
                    break
                chunks.append(chunk)
            body = b"".join(chunks)
            status_code = resp.status_code
            final_url = str(resp.url)

    body = _TAG_SCRIPT.sub(b" ", body)
    body = _TAG.sub(b" ", body)
    text = _WS.sub(b" ", body).decode("utf-8", errors="replace").strip()
    truncated = False
    if len(text) > max_chars:
        text = text[:max_chars] + f"\n[... truncated at {max_chars} chars]"
        truncated = True
    if size_truncated:
        text += f"\n[... response body truncated at {max_bytes} bytes]"
        truncated = True
    # Mask secrets in the fetched content.
    safe_text = mask_tool_output(text or "(empty body)")
    return ToolResult.ok(
        safe_text,
        final_url=final_url,
        status_code=status_code,
        truncated=truncated,
        size_truncated=size_truncated,
    )


def register_web_tools() -> None:
    register_tool(
        name="web_search",
        description=(
            "Search the web for a query and return the top results "
            "(title, URL, snippet). Provider is configured server-side."
        ),
        args_model=WebSearchArgs,
        func=web_search,
        capabilities=frozenset({Capability.NETWORK}),
    )
    register_tool(
        name="web_fetch",
        description=(
            "Download a URL and return its main text content with HTML tags "
            "stripped. SSRF-protected: private IPs are blocked and a domain "
            "allowlist may be configured. Useful for reading an article "
            "returned by web_search."
        ),
        args_model=WebFetchArgs,
        func=web_fetch,
        capabilities=frozenset({Capability.NETWORK}),
    )
