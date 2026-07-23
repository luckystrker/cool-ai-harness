"""Tests for sandbox environment preparation (Фаза 1.5 §2)."""

from __future__ import annotations

import os

import pytest

from app.security.sandbox import build_sandbox_env


@pytest.fixture
def fake_env(monkeypatch: pytest.MonkeyPatch) -> dict[str, str]:
    """Set up a controlled environment for testing."""
    env = {
        "PATH": "/usr/bin:/bin",
        "PYTHONPATH": "/usr/lib/python3",
        "SYSTEMROOT": "C:\\Windows",
        "OPENAI_API_KEY": "sk-test1234567890abcdef",
        "ANTHROPIC_API_KEY": "sk-ant-test",
        "MY_API_KEY": "some-secret-value",
        "DATABASE_PASSWORD": "supersecret",
        "AUTH_TOKEN": "bearer-token-value",
        "SECRET_KEY": "fernet-key-value",
        "AWS_ACCESS_KEY_ID": "AKIAIOSFODNN7EXAMPLE",
        "AWS_SECRET_ACCESS_KEY": "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "HOME": "/home/user",
        "USERNAME": "testuser",
        "LANG": "en_US.UTF-8",
        "MY_CUSTOM_VAR": "harmless-value",
    }
    monkeypatch.setattr(os, "environ", env)
    return env


class TestBuildSandboxEnv:
    def test_strip_secrets_false_returns_full_env(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env(strip_secrets=False)
        assert result == fake_env

    def test_strips_known_api_keys(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env()
        assert "OPENAI_API_KEY" not in result
        assert "ANTHROPIC_API_KEY" not in result

    def test_strips_pattern_matched_keys(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env()
        # MY_API_KEY matches .*_?API[_-]?KEY$
        assert "MY_API_KEY" not in result
        # DATABASE_PASSWORD matches .*_?PASSWORD$
        assert "DATABASE_PASSWORD" not in result
        # AUTH_TOKEN matches .*_?TOKEN$
        assert "AUTH_TOKEN" not in result
        # SECRET_KEY matches ^SECRET_KEY$
        assert "SECRET_KEY" not in result

    def test_strips_aws_credentials(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env()
        assert "AWS_ACCESS_KEY_ID" not in result
        assert "AWS_SECRET_ACCESS_KEY" not in result

    def test_keeps_safe_system_vars(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env()
        assert result["PATH"] == "/usr/bin:/bin"
        assert result["PYTHONPATH"] == "/usr/lib/python3"
        assert result["SYSTEMROOT"] == "C:\\Windows"
        assert result["HOME"] == "/home/user"
        assert result["LANG"] == "en_US.UTF-8"

    def test_keeps_non_secret_non_system_vars(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env()
        assert result["MY_CUSTOM_VAR"] == "harmless-value"

    def test_extra_allow_whitelists_specific_var(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env(extra_allow=["OPENAI_API_KEY"])
        assert "OPENAI_API_KEY" in result
        assert result["OPENAI_API_KEY"] == "sk-test1234567890abcdef"
        # Other secrets are still stripped
        assert "ANTHROPIC_API_KEY" not in result

    def test_extra_allow_case_insensitive(self, fake_env: dict[str, str]) -> None:
        result = build_sandbox_env(extra_allow=["openai_api_key"])
        assert "OPENAI_API_KEY" in result
