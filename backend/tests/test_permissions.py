"""Tests for tool permission resolution (app/agent/permissions.py)."""

from __future__ import annotations

from app.agent.permissions import (
    PermissionsConfig,
    merge,
    normalize,
    validate,
)


class TestResolve:
    def test_explicit_tool_decision_wins(self) -> None:
        cfg = PermissionsConfig(tools={"*": "allow", "read_file": "deny"})
        assert cfg.resolve("read_file") == "deny"

    def test_wildcard_used_when_no_explicit(self) -> None:
        cfg = PermissionsConfig(tools={"*": "allow"})
        assert cfg.resolve("python_execute") == "allow"

    def test_safe_tool_falls_back_to_allow_when_nothing_configured(self) -> None:
        cfg = PermissionsConfig(tools={})
        # Read-only tools run straight through by default — no approval dialog,
        # no 30s wait. This is what makes web_search / read_file work out of
        # the box without configuring default_tool_permissions.
        assert cfg.resolve("read_file") == "allow"
        assert cfg.resolve("web_search") == "allow"

    def test_dangerous_tool_falls_back_to_ask_when_nothing_configured(self) -> None:
        cfg = PermissionsConfig(tools={})
        # Side-effecting tools (code execution, destructive ops) still prompt.
        assert cfg.resolve("python_execute", dangerous=True) == "ask"

    def test_dangerous_flag_does_not_override_explicit_allow(self) -> None:
        cfg = PermissionsConfig(tools={"python_execute": "allow"})
        assert cfg.resolve("python_execute", dangerous=True) == "allow"

    def test_invalid_decision_falls_back_to_ask(self) -> None:
        cfg = PermissionsConfig(tools={"read_file": "bogus"})
        assert cfg.resolve("read_file") == "ask"


class TestNormalize:
    def test_strips_lowercases_and_drops_invalid(self) -> None:
        # Tool-name keys are preserved as-is (registry names are case-sensitive);
        # only decision *values* are normalized, and invalid entries dropped.
        out = normalize({"read_file": "ALLOW", "bad": "nope", "*": "Ask"})
        assert out == {"read_file": "allow", "*": "ask"}

    def test_none_input_returns_empty(self) -> None:
        assert normalize(None) == {}


class TestValidate:
    def test_none_is_valid(self) -> None:
        assert validate(None) == []

    def test_valid_entries_no_errors(self) -> None:
        assert validate({"*": "allow", "x": "deny"}) == []

    def test_invalid_entry_reported(self) -> None:
        errs = validate({"x": "maybe"})
        assert len(errs) == 1
        assert "allow|ask|deny" in errs[0]

    def test_non_dict_reported(self) -> None:
        errs = validate(["allow"])  # type: ignore[arg-type]
        assert len(errs) == 1


class TestMerge:
    def test_conversation_overrides_global(self) -> None:
        cfg = merge({"*": "ask", "read_file": "deny"}, {"read_file": "allow"})
        assert cfg.resolve("read_file") == "allow"

    def test_global_used_when_conversation_silent(self) -> None:
        cfg = merge({"*": "allow"}, None)
        assert cfg.resolve("anything") == "allow"

    def test_conversation_wildcard_overrides_global_wildcard(self) -> None:
        cfg = merge({"*": "ask"}, {"*": "allow"})
        assert cfg.resolve("anything") == "allow"

    def test_conversation_explicit_beats_global_wildcard(self) -> None:
        # global says everything is allowed; conversation pins one to deny.
        cfg = merge({"*": "allow"}, {"write_file": "deny"})
        assert cfg.resolve("write_file") == "deny"
        assert cfg.resolve("read_file") == "allow"

    def test_invalid_entries_in_either_map_dropped(self) -> None:
        cfg = merge({"*": "ask", "bad": "nope"}, {"also_bad": 123})  # type: ignore[dict-item]
        # Bad entries dropped, "*" preserved.
        assert cfg.resolve("anything") == "ask"
