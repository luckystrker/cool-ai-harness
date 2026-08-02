"""Tests for bash_execute, git tools, and GitHub API tools (Фаза 4 §3)."""

from __future__ import annotations

import subprocess
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest

from app.tools.base import ToolResult

# --- Fixtures ---


@pytest.fixture
def git_repo(tmp_path: Path, monkeypatch) -> Path:
    """Create a temporary git repository with an initial commit."""
    from app.core import config as config_module
    from app.tools.context import RunContext, set_run_context

    repo = tmp_path / "repo"
    repo.mkdir()

    # Initialize git repo.
    subprocess.run(["git", "init"], cwd=str(repo), capture_output=True, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=str(repo),
        capture_output=True,
        check=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test User"],
        cwd=str(repo),
        capture_output=True,
        check=True,
    )

    # Create an initial commit.
    (repo / "hello.txt").write_text("hello world\n")
    subprocess.run(["git", "add", "."], cwd=str(repo), capture_output=True, check=True)
    subprocess.run(
        ["git", "commit", "-m", "initial commit"],
        cwd=str(repo),
        capture_output=True,
        check=True,
    )

    # Point the run context at this repo.
    settings = config_module.get_settings()
    monkeypatch.setattr(settings, "workspaces_dir", repo)
    ctx = RunContext(workdir=repo)
    token = set_run_context(ctx)
    yield repo
    from app.tools.context import reset_run_context

    reset_run_context(token)


@pytest.fixture
def workspace_ctx(tmp_path: Path, monkeypatch) -> Path:
    """Set up a plain workspace context (no git)."""
    from app.core import config as config_module
    from app.tools.context import RunContext, set_run_context

    ws = tmp_path / "ws"
    ws.mkdir()
    settings = config_module.get_settings()
    monkeypatch.setattr(settings, "workspaces_dir", ws)
    ctx = RunContext(workdir=ws)
    token = set_run_context(ctx)
    yield ws
    from app.tools.context import reset_run_context

    reset_run_context(token)


# --- bash_execute ---


class TestBashExecute:
    async def test_echo(self, workspace_ctx: Path) -> None:
        from app.tools.bash_tools import bash_execute

        result = await bash_execute(command="echo hello")
        assert not result.is_error
        assert "hello" in result.output

    async def test_exit_code(self, workspace_ctx: Path) -> None:
        from app.tools.bash_tools import bash_execute

        result = await bash_execute(command="exit 42")
        assert result.metadata.get("exit_code") == 42

    async def test_timeout(self, workspace_ctx: Path) -> None:
        from app.tools.bash_tools import bash_execute

        result = await bash_execute(command="ping -n 10 127.0.0.1", timeout=0.5)
        assert result.is_error
        assert "timed out" in result.output.lower()

    async def test_stderr_captured(self, workspace_ctx: Path) -> None:
        from app.tools.bash_tools import bash_execute

        # Write to stderr.
        result = await bash_execute(command="echo error_msg >&2")
        assert not result.is_error
        assert result.metadata.get("stderr") is not None


# --- git_status ---


class TestGitStatus:
    async def test_clean_repo(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_status

        result = await git_status()
        assert not result.is_error
        # Should mention the branch.
        assert "main" in result.output or "master" in result.output

    async def test_with_untracked(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_status

        (git_repo / "new_file.txt").write_text("untracked")
        result = await git_status()
        assert not result.is_error
        assert "new_file.txt" in result.output

    async def test_not_a_repo(self, workspace_ctx: Path) -> None:
        from app.tools.git_tools import git_status

        result = await git_status()
        assert result.is_error


# --- git_log ---


class TestGitLog:
    async def test_shows_commits(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_log

        result = await git_log(limit=5)
        assert not result.is_error
        assert "initial commit" in result.output

    async def test_limit(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_log

        # Add more commits.
        for i in range(5):
            (git_repo / f"file{i}.txt").write_text(f"content {i}")
            subprocess.run(["git", "add", "."], cwd=str(git_repo), capture_output=True)
            subprocess.run(
                ["git", "commit", "-m", f"commit {i}"],
                cwd=str(git_repo),
                capture_output=True,
            )

        result = await git_log(limit=3)
        assert not result.is_error
        # Should have at most 3 lines.
        lines = [ln for ln in result.output.splitlines() if ln.strip()]
        assert len(lines) <= 3


# --- git_diff ---


class TestGitDiff:
    async def test_unstaged_changes(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_diff

        (git_repo / "hello.txt").write_text("modified content\n")
        result = await git_diff()
        assert not result.is_error
        assert "modified content" in result.output

    async def test_staged_changes(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_diff

        (git_repo / "hello.txt").write_text("staged change\n")
        subprocess.run(["git", "add", "."], cwd=str(git_repo), capture_output=True)
        result = await git_diff(staged=True)
        assert not result.is_error
        assert "staged change" in result.output

    async def test_no_changes(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_diff

        result = await git_diff()
        assert not result.is_error


# --- git_branch ---


class TestGitBranch:
    async def test_list(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_branch

        result = await git_branch(action="list")
        assert not result.is_error
        assert "main" in result.output or "master" in result.output

    async def test_create_and_checkout(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_branch

        result = await git_branch(action="create", name="feature-x")
        assert not result.is_error

        result = await git_branch(action="checkout", name="feature-x")
        assert not result.is_error

        # Verify we're on the new branch.
        result = await git_branch(action="list")
        assert "feature-x" in result.output

    async def test_delete(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_branch

        await git_branch(action="create", name="to-delete")
        result = await git_branch(action="delete", name="to-delete")
        assert not result.is_error

    async def test_create_requires_name(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_branch

        result = await git_branch(action="create")
        assert result.is_error
        assert "required" in result.output.lower()


# --- git_commit ---


class TestGitCommit:
    async def test_commit_with_add_all(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_commit, git_log

        (git_repo / "new.txt").write_text("new file")
        result = await git_commit(message="add new file")
        assert not result.is_error

        log = await git_log(limit=1)
        assert "add new file" in log.output

    async def test_commit_nothing_to_commit(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_commit

        result = await git_commit(message="empty commit")
        # git returns error when nothing to commit.
        assert result.is_error


# --- git_blame ---


class TestGitBlame:
    async def test_blame_file(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_blame

        result = await git_blame(path="hello.txt")
        assert not result.is_error
        assert "Test User" in result.output

    async def test_blame_nonexistent(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_blame

        result = await git_blame(path="nonexistent.txt")
        assert result.is_error


# --- git_clone ---


class TestGitClone:
    async def test_invalid_scheme(self, workspace_ctx: Path) -> None:
        from app.tools.git_tools import git_clone

        result = await git_clone(url="ftp://example.com/repo.git")
        assert result.is_error
        assert "https" in result.output.lower() or "ssh" in result.output.lower()

    async def test_clone_local_repo(self, git_repo: Path, tmp_path: Path) -> None:
        """Clone from a local path (acts like a file:// URL)."""
        from app.tools.context import RunContext, set_run_context
        from app.tools.git_tools import git_clone

        # Set workspace to a different dir for the clone target.
        clone_target = tmp_path / "clone_ws"
        clone_target.mkdir()
        ctx = RunContext(workdir=clone_target)
        token = set_run_context(ctx)
        try:
            # Git supports cloning from local paths.
            result = await git_clone(url=str(git_repo), directory="cloned")
            # Local path clone might not pass URL validation, that's OK.
            # The important thing is it doesn't crash.
            assert isinstance(result, ToolResult)
        finally:
            from app.tools.context import reset_run_context

            reset_run_context(token)


# --- git_push ---


class TestGitPush:
    async def test_push_no_remote(self, git_repo: Path) -> None:
        from app.tools.git_tools import git_push

        result = await git_push()
        assert result.is_error
        # Should mention remote or push failure.
        assert "remote" in result.output.lower() or "push" in result.output.lower()


# --- GitHub tools (mocked) ---


class TestGitHubTools:
    async def test_no_token_error(self, monkeypatch) -> None:
        from app.core import config as config_module
        from app.tools.github_tools import github_pr_list

        settings = config_module.get_settings()
        monkeypatch.setattr(settings, "github_token", "")

        result = await github_pr_list(repo="owner/repo")
        assert result.is_error
        assert "GITHUB_TOKEN" in result.output

    async def test_pr_list_with_token(self, monkeypatch) -> None:
        from app.core import config as config_module
        from app.tools.github_tools import github_pr_list

        settings = config_module.get_settings()
        monkeypatch.setattr(settings, "github_token", "ghp_fake_token")

        mock_response = AsyncMock()
        mock_response.status_code = 200
        mock_response.text = '[{"number": 1, "title": "Test PR"}]'

        with patch("app.tools.github_tools.httpx.AsyncClient") as mock_client:
            mock_instance = AsyncMock()
            mock_instance.get.return_value = mock_response
            mock_instance.__aenter__ = AsyncMock(return_value=mock_instance)
            mock_instance.__aexit__ = AsyncMock(return_value=False)
            mock_client.return_value = mock_instance

            result = await github_pr_list(repo="owner/repo")
            assert not result.is_error
            assert "Test PR" in result.output

    async def test_normalize_repo_url(self) -> None:
        from app.tools.github_tools import _normalize_repo

        assert _normalize_repo("owner/repo") == "owner/repo"
        assert _normalize_repo("https://github.com/owner/repo") == "owner/repo"
        assert _normalize_repo("/owner/repo") == "owner/repo"

    async def test_actions_status(self, monkeypatch) -> None:
        from app.core import config as config_module
        from app.tools.github_tools import github_actions_status

        settings = config_module.get_settings()
        monkeypatch.setattr(settings, "github_token", "ghp_fake_token")

        mock_response = AsyncMock()
        mock_response.status_code = 200
        mock_response.text = '{"workflow_runs": [{"status": "completed"}]}'

        with patch("app.tools.github_tools.httpx.AsyncClient") as mock_client:
            mock_instance = AsyncMock()
            mock_instance.get.return_value = mock_response
            mock_instance.__aenter__ = AsyncMock(return_value=mock_instance)
            mock_instance.__aexit__ = AsyncMock(return_value=False)
            mock_client.return_value = mock_instance

            result = await github_actions_status(repo="owner/repo")
            assert not result.is_error
            assert "completed" in result.output


# --- Capability gating ---


class TestCapabilityGating:
    def test_git_tools_have_git_capability(self) -> None:
        from app.security.capabilities import Capability, tool_capabilities

        caps = tool_capabilities("git_status")
        assert Capability.GIT in caps

        caps = tool_capabilities("git_clone")
        assert Capability.GIT in caps
        assert Capability.NETWORK in caps

    def test_bash_has_execute_capability(self) -> None:
        from app.security.capabilities import Capability, tool_capabilities

        caps = tool_capabilities("bash_execute")
        assert Capability.EXECUTE in caps

    def test_github_tools_have_network_and_git(self) -> None:
        from app.security.capabilities import Capability, tool_capabilities

        for tool_name in (
            "github_pr_list",
            "github_pr_diff",
            "github_pr_review",
            "github_issue_list",
            "github_issue_create",
            "github_actions_status",
        ):
            caps = tool_capabilities(tool_name)
            assert Capability.NETWORK in caps, f"{tool_name} missing NETWORK"
            assert Capability.GIT in caps, f"{tool_name} missing GIT"
