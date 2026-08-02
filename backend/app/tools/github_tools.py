"""GitHub API tools: PR review, issues, and Actions status.

Uses the GitHub REST API v3 (``https://api.github.com``) with a Personal
Access Token configured via ``GITHUB_TOKEN`` in settings. All tools require
both ``Capability.NETWORK`` and ``Capability.GIT``.

Tools degrade gracefully when no token is configured — they return an
informative error rather than crashing.
"""

from __future__ import annotations

from typing import Any, Literal

import httpx

from app.core.config import get_settings
from app.security.capabilities import Capability
from app.tools.base import ToolArgs, ToolResult, register_tool

_GITHUB_API = "https://api.github.com"
_TIMEOUT = 30.0


def _headers() -> dict[str, str] | None:
    """Build auth headers. Returns None if no token is configured."""
    settings = get_settings()
    token = settings.github_token
    if not token:
        return None
    return {
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    }


def _no_token_error() -> ToolResult:
    return ToolResult.err(
        "GITHUB_TOKEN is not configured. Set it in .env to use GitHub API tools."
    )


async def _gh_get(path: str, *, accept: str | None = None) -> ToolResult:
    """Perform a GET request to the GitHub API."""
    headers = _headers()
    if headers is None:
        return _no_token_error()
    if accept:
        headers = {**headers, "Accept": accept}
    try:
        async with httpx.AsyncClient(timeout=_TIMEOUT) as client:
            resp = await client.get(f"{_GITHUB_API}{path}", headers=headers)
        if resp.status_code >= 400:
            return ToolResult.err(
                f"GitHub API error {resp.status_code}: {resp.text[:500]}",
                status_code=resp.status_code,
            )
        return ToolResult.ok(resp.text[:50_000])
    except httpx.HTTPError as exc:
        return ToolResult.err(f"GitHub API request failed: {exc}")


async def _gh_post(path: str, *, json: dict[str, Any]) -> ToolResult:
    """Perform a POST request to the GitHub API."""
    headers = _headers()
    if headers is None:
        return _no_token_error()
    try:
        async with httpx.AsyncClient(timeout=_TIMEOUT) as client:
            resp = await client.post(f"{_GITHUB_API}{path}", headers=headers, json=json)
        if resp.status_code >= 400:
            return ToolResult.err(
                f"GitHub API error {resp.status_code}: {resp.text[:500]}",
                status_code=resp.status_code,
            )
        return ToolResult.ok(resp.text[:50_000])
    except httpx.HTTPError as exc:
        return ToolResult.err(f"GitHub API request failed: {exc}")


def _normalize_repo(repo: str) -> str:
    """Ensure repo is in owner/name format (strip leading slash or URL)."""
    repo = repo.strip().rstrip("/")
    if "github.com/" in repo:
        repo = repo.split("github.com/")[-1]
    return repo.lstrip("/")


# --- github_pr_list --------------------------------------------------------


class GitHubPRListArgs(ToolArgs):
    repo: str
    state: str = "open"


async def github_pr_list(*, repo: str, state: str = "open") -> ToolResult:
    """List pull requests for a repository."""
    repo = _normalize_repo(repo)
    return await _gh_get(f"/repos/{repo}/pulls?state={state}&per_page=30")


# --- github_pr_diff --------------------------------------------------------


class GitHubPRDiffArgs(ToolArgs):
    repo: str
    pr_number: int


async def github_pr_diff(*, repo: str, pr_number: int) -> ToolResult:
    """Get the diff for a pull request."""
    repo = _normalize_repo(repo)
    return await _gh_get(
        f"/repos/{repo}/pulls/{pr_number}",
        accept="application/vnd.github.diff",
    )


# --- github_pr_review ------------------------------------------------------


class GitHubPRReviewArgs(ToolArgs):
    repo: str
    pr_number: int
    body: str
    event: Literal["APPROVE", "REQUEST_CHANGES", "COMMENT"] = "COMMENT"


async def github_pr_review(
    *, repo: str, pr_number: int, body: str, event: str = "COMMENT"
) -> ToolResult:
    """Submit a review on a pull request."""
    repo = _normalize_repo(repo)
    return await _gh_post(
        f"/repos/{repo}/pulls/{pr_number}/reviews",
        json={"body": body, "event": event},
    )


# --- github_issue_list -----------------------------------------------------


class GitHubIssueListArgs(ToolArgs):
    repo: str
    state: str = "open"
    labels: str | None = None


async def github_issue_list(
    *, repo: str, state: str = "open", labels: str | None = None
) -> ToolResult:
    """List issues for a repository."""
    repo = _normalize_repo(repo)
    path = f"/repos/{repo}/issues?state={state}&per_page=30"
    if labels:
        path += f"&labels={labels}"
    return await _gh_get(path)


# --- github_issue_create ---------------------------------------------------


class GitHubIssueCreateArgs(ToolArgs):
    repo: str
    title: str
    body: str
    labels: list[str] | None = None


async def github_issue_create(
    *, repo: str, title: str, body: str, labels: list[str] | None = None
) -> ToolResult:
    """Create a new issue in a repository."""
    repo = _normalize_repo(repo)
    payload: dict[str, Any] = {"title": title, "body": body}
    if labels:
        payload["labels"] = labels
    return await _gh_post(f"/repos/{repo}/issues", json=payload)


# --- github_actions_status -------------------------------------------------


class GitHubActionsStatusArgs(ToolArgs):
    repo: str
    branch: str | None = None


async def github_actions_status(*, repo: str, branch: str | None = None) -> ToolResult:
    """Get recent workflow run statuses for a repository."""
    repo = _normalize_repo(repo)
    path = f"/repos/{repo}/actions/runs?per_page=10"
    if branch:
        path += f"&branch={branch}"
    return await _gh_get(path)


# --- Registration ----------------------------------------------------------

_GH_CAPS = frozenset({Capability.NETWORK, Capability.GIT})


def register_github_tools() -> None:
    register_tool(
        name="github_pr_list",
        description=(
            "List pull requests for a GitHub repository. Returns PR numbers, "
            "titles, authors, and states. Requires GITHUB_TOKEN."
        ),
        args_model=GitHubPRListArgs,
        func=github_pr_list,
        capabilities=_GH_CAPS,
    )
    register_tool(
        name="github_pr_diff",
        description=(
            "Get the unified diff for a GitHub pull request. Useful for "
            "reviewing code changes. Requires GITHUB_TOKEN."
        ),
        args_model=GitHubPRDiffArgs,
        func=github_pr_diff,
        capabilities=_GH_CAPS,
    )
    register_tool(
        name="github_pr_review",
        description=(
            "Submit a review on a GitHub pull request (APPROVE, "
            "REQUEST_CHANGES, or COMMENT). Requires GITHUB_TOKEN."
        ),
        args_model=GitHubPRReviewArgs,
        func=github_pr_review,
        capabilities=_GH_CAPS,
    )
    register_tool(
        name="github_issue_list",
        description=(
            "List issues for a GitHub repository. Filter by state and labels. "
            "Requires GITHUB_TOKEN."
        ),
        args_model=GitHubIssueListArgs,
        func=github_issue_list,
        capabilities=_GH_CAPS,
    )
    register_tool(
        name="github_issue_create",
        description=(
            "Create a new issue in a GitHub repository with title, body, and "
            "optional labels. Requires GITHUB_TOKEN."
        ),
        args_model=GitHubIssueCreateArgs,
        func=github_issue_create,
        capabilities=_GH_CAPS,
    )
    register_tool(
        name="github_actions_status",
        description=(
            "Get recent GitHub Actions workflow run statuses for a repository. "
            "Optionally filter by branch. Requires GITHUB_TOKEN."
        ),
        args_model=GitHubActionsStatusArgs,
        func=github_actions_status,
        capabilities=_GH_CAPS,
    )
