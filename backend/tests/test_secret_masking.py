"""Tests for secret masking (Фаза 1.5 §2)."""

from __future__ import annotations

from app.security.secrets import mask_secrets, mask_secrets_in_value


class TestMaskSecretsBasic:
    def test_disabled_returns_unchanged(self) -> None:
        text = "Bearer sk-1234567890abcdef1234567890"
        assert mask_secrets(text, enabled=False) == text

    def test_empty_string(self) -> None:
        assert mask_secrets("") == ""

    def test_no_secrets_unchanged(self) -> None:
        text = "This is a normal text with no secrets."
        assert mask_secrets(text) == text

    def test_bearer_token_masked(self) -> None:
        text = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIx"
        result = mask_secrets(text)
        assert "[REDACTED:bearer]" in result
        assert "eyJhbGciOiJIUzI1NiJ9" not in result

    def test_api_key_masked(self) -> None:
        text = "Using sk-1234567890abcdef1234567890 for the API"
        result = mask_secrets(text)
        assert "[REDACTED:api-key]" in result
        assert "1234567890abcdef1234567890" not in result

    def test_github_token_masked(self) -> None:
        text = "Token: ghp_1234567890abcdef1234567890abcdef1234"
        result = mask_secrets(text)
        assert "[REDACTED:github-token]" in result

    def test_slack_token_masked(self) -> None:
        text = "Token: xoxb-1234567890-abcdef"
        result = mask_secrets(text)
        assert "[REDACTED:slack-token]" in result

    def test_aws_key_masked(self) -> None:
        text = "AKIAIOSFODNN7EXAMPLE"
        result = mask_secrets(text)
        assert "[REDACTED:aws-key]" in result

    def test_pem_key_masked(self) -> None:
        text = (
            "-----BEGIN RSA PRIVATE KEY-----\n"
            "MIIEpAIBAAKCAQEA...\n"
            "-----END RSA PRIVATE KEY-----"
        )
        result = mask_secrets(text)
        assert "[REDACTED:private-key]" in result
        assert "MIIEpAIBAAKCAQEA" not in result

    def test_env_assignment_masked(self) -> None:
        text = "API_KEY=supersecretvalue123"
        result = mask_secrets(text)
        assert "[REDACTED]" in result
        assert "supersecretvalue123" not in result

    def test_password_assignment_masked(self) -> None:
        text = "password=mysecretpass123"
        result = mask_secrets(text)
        assert "[REDACTED]" in result
        assert "mysecretpass123" not in result

    def test_already_redacted_not_reprocessed(self) -> None:
        text = "[REDACTED:api-key] and [REDACTED:bearer]"
        result = mask_secrets(text)
        assert result == text


class TestMaskSecretsInValue:
    def test_string_passthrough(self) -> None:
        result = mask_secrets_in_value("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIx")
        assert "[REDACTED:bearer]" in result

    def test_dict_recursion(self) -> None:
        data = {"key": "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIx", "safe": "hello"}
        result = mask_secrets_in_value(data)
        assert isinstance(result, dict)
        assert "[REDACTED:bearer]" in result["key"]
        assert result["safe"] == "hello"

    def test_list_recursion(self) -> None:
        data = ["Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIx", "normal"]
        result = mask_secrets_in_value(data)
        assert isinstance(result, list)
        assert "[REDACTED:bearer]" in result[0]
        assert result[1] == "normal"

    def test_nested_structures(self) -> None:
        data = {
            "outer": {
                "inner": ["Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIx"],
                "nested_key": "sk-1234567890abcdef1234567890",
            }
        }
        result = mask_secrets_in_value(data)
        assert "[REDACTED:bearer]" in result["outer"]["inner"][0]
        assert "[REDACTED:api-key]" in result["outer"]["nested_key"]

    def test_non_string_values_passthrough(self) -> None:
        data = {"count": 42, "active": True, "items": [1, 2, 3]}
        result = mask_secrets_in_value(data)
        assert result == data

    def test_disabled_returns_unchanged(self) -> None:
        data = {"key": "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIx"}
        result = mask_secrets_in_value(data, enabled=False)
        assert result == data
