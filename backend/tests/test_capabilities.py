"""Tests for the capability security layer (Фаза 1.5 §2).

Covers capability resolution, policy merging, validation, and the
stricter() comparison function.
"""

from __future__ import annotations

from app.security.capabilities import (
    Capability,
    CapabilityPolicy,
    merge_policy,
    normalize_policy,
    stricter,
    validate_policy,
)


class TestCapabilityPolicy:
    def test_empty_policy_allows_everything(self) -> None:
        policy = CapabilityPolicy(caps={})
        assert policy.resolve(Capability.READ) == "allow"
        assert policy.resolve(Capability.EXECUTE) == "allow"
        assert policy.resolve(Capability.NETWORK) == "allow"

    def test_explicit_capability_entry(self) -> None:
        policy = CapabilityPolicy(caps={"execute": "deny"})
        assert policy.resolve(Capability.EXECUTE) == "deny"
        assert policy.resolve(Capability.READ) == "allow"

    def test_wildcard_fallback(self) -> None:
        policy = CapabilityPolicy(caps={"*": "ask"})
        assert policy.resolve(Capability.READ) == "ask"
        assert policy.resolve(Capability.EXECUTE) == "ask"
        assert policy.resolve(Capability.NETWORK) == "ask"

    def test_explicit_overrides_wildcard(self) -> None:
        policy = CapabilityPolicy(caps={"*": "ask", "read": "allow"})
        assert policy.resolve(Capability.READ) == "allow"
        assert policy.resolve(Capability.EXECUTE) == "ask"

    def test_resolve_tool_returns_most_restrictive(self) -> None:
        # python_execute has EXECUTE capability only
        policy = CapabilityPolicy(caps={"execute": "ask"})
        assert policy.resolve_tool("python_execute") == "ask"

    def test_resolve_tool_unknown_tool_allows(self) -> None:
        policy = CapabilityPolicy(caps={"*": "deny"})
        # Unknown tools have no capabilities → allow (opt-in)
        assert policy.resolve_tool("nonexistent_tool") == "allow"

    def test_resolve_tool_deny_takes_precedence(self) -> None:
        # write_file has WRITE capability
        policy = CapabilityPolicy(caps={"write": "deny", "*": "allow"})
        assert policy.resolve_tool("write_file") == "deny"

    def test_resolve_tool_ask_vs_allow(self) -> None:
        policy = CapabilityPolicy(caps={"read": "allow", "*": "ask"})
        # read_file has READ only → allow (from explicit read)
        assert policy.resolve_tool("read_file") == "allow"
        # web_fetch has NETWORK → ask (from wildcard)
        assert policy.resolve_tool("web_fetch") == "ask"


class TestStricter:
    def test_allow_vs_allow(self) -> None:
        assert stricter("allow", "allow") == "allow"

    def test_allow_vs_ask(self) -> None:
        assert stricter("allow", "ask") == "ask"
        assert stricter("ask", "allow") == "ask"

    def test_allow_vs_deny(self) -> None:
        assert stricter("allow", "deny") == "deny"
        assert stricter("deny", "allow") == "deny"

    def test_ask_vs_deny(self) -> None:
        assert stricter("ask", "deny") == "deny"
        assert stricter("deny", "ask") == "deny"

    def test_ask_vs_ask(self) -> None:
        assert stricter("ask", "ask") == "ask"

    def test_deny_vs_deny(self) -> None:
        assert stricter("deny", "deny") == "deny"


class TestNormalizeAndValidate:
    def test_normalize_none_returns_empty(self) -> None:
        assert normalize_policy(None) == {}

    def test_normalize_strips_and_lowercases(self) -> None:
        result = normalize_policy({"EXECUTE": "ASK", "Read": "Allow"})
        # Keys are preserved; values are lowercased.
        assert result == {"EXECUTE": "ask", "Read": "allow"}

    def test_normalize_skips_invalid(self) -> None:
        result = normalize_policy({"execute": "maybe", "read": "allow"})
        assert result == {"read": "allow"}

    def test_validate_none_returns_empty(self) -> None:
        assert validate_policy(None) == []

    def test_validate_valid_returns_empty(self) -> None:
        assert validate_policy({"execute": "ask", "read": "allow"}) == []

    def test_validate_invalid_returns_errors(self) -> None:
        errors = validate_policy({"execute": "maybe"})
        assert len(errors) == 1
        assert "allow|ask|deny" in errors[0]

    def test_validate_non_dict_returns_error(self) -> None:
        errors = validate_policy("not a dict")  # type: ignore[arg-type]
        assert len(errors) == 1


class TestMergePolicy:
    def test_merge_global_and_conversation(self) -> None:
        policy = merge_policy(
            {"execute": "ask", "network": "ask"},
            {"network": "allow"},
        )
        assert policy.resolve(Capability.EXECUTE) == "ask"
        assert policy.resolve(Capability.NETWORK) == "allow"

    def test_merge_empty_conversation_uses_global(self) -> None:
        policy = merge_policy({"*": "ask"}, None)
        assert policy.resolve(Capability.READ) == "ask"

    def test_merge_conversation_wildcard_wins(self) -> None:
        policy = merge_policy({"*": "ask"}, {"*": "allow"})
        assert policy.resolve(Capability.READ) == "allow"
        assert policy.resolve(Capability.EXECUTE) == "allow"

    def test_merge_both_empty(self) -> None:
        policy = merge_policy(None, None)
        assert policy.resolve(Capability.READ) == "allow"
        assert policy.resolve_tool("python_execute") == "allow"
