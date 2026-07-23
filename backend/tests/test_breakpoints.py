"""Tests for HITL breakpoints (Фаза 1.5 §2)."""

from __future__ import annotations

from app.security.breakpoints import (
    BreakpointConfig,
    BreakpointsConfig,
    BreakpointType,
    is_write_tool,
    merge_breakpoints,
    parse_breakpoints,
)


class TestParseBreakpoints:
    def test_none_returns_empty(self) -> None:
        config = parse_breakpoints(None)
        assert config.is_empty

    def test_empty_list_returns_empty(self) -> None:
        config = parse_breakpoints([])
        assert config.is_empty

    def test_single_breakpoint(self) -> None:
        config = parse_breakpoints([{"type": "before_tool"}])
        assert not config.is_empty
        assert len(config.breakpoints) == 1
        assert config.breakpoints[0].bp_type == BreakpointType.BEFORE_TOOL

    def test_breakpoint_with_tool_filter(self) -> None:
        config = parse_breakpoints([{"type": "before_write", "tool": "write_file"}])
        bp = config.breakpoints[0]
        assert bp.bp_type == BreakpointType.BEFORE_WRITE
        assert bp.tool_name == "write_file"

    def test_breakpoint_with_ttl_and_fallback(self) -> None:
        config = parse_breakpoints([
            {"type": "before_tool", "ttl_s": 10.0, "fallback": "skip"}
        ])
        bp = config.breakpoints[0]
        assert bp.ttl_s == 10.0
        assert bp.fallback == "skip"

    def test_invalid_type_skipped(self) -> None:
        config = parse_breakpoints([{"type": "invalid_type"}, {"type": "before_tool"}])
        assert len(config.breakpoints) == 1
        assert config.breakpoints[0].bp_type == BreakpointType.BEFORE_TOOL

    def test_invalid_entry_skipped(self) -> None:
        config = parse_breakpoints(["not-a-dict", {"type": "before_tool"}])
        assert len(config.breakpoints) == 1

    def test_default_fallback_is_deny(self) -> None:
        config = parse_breakpoints([{"type": "before_tool"}])
        assert config.breakpoints[0].fallback == "deny"


class TestShouldBreak:
    def test_no_breakpoints_returns_none(self) -> None:
        config = BreakpointsConfig()
        assert config.should_break(BreakpointType.BEFORE_TOOL) is None

    def test_matching_type_returns_config(self) -> None:
        config = BreakpointsConfig(breakpoints=[
            BreakpointConfig(bp_type=BreakpointType.BEFORE_TOOL)
        ])
        result = config.should_break(BreakpointType.BEFORE_TOOL)
        assert result is not None
        assert result.bp_type == BreakpointType.BEFORE_TOOL

    def test_non_matching_type_returns_none(self) -> None:
        config = BreakpointsConfig(breakpoints=[
            BreakpointConfig(bp_type=BreakpointType.BEFORE_WRITE)
        ])
        assert config.should_break(BreakpointType.BEFORE_TOOL) is None

    def test_tool_filter_matches(self) -> None:
        config = BreakpointsConfig(breakpoints=[
            BreakpointConfig(bp_type=BreakpointType.BEFORE_TOOL, tool_name="write_file")
        ])
        result = config.should_break(BreakpointType.BEFORE_TOOL, tool_name="write_file")
        assert result is not None

    def test_tool_filter_does_not_match(self) -> None:
        config = BreakpointsConfig(breakpoints=[
            BreakpointConfig(bp_type=BreakpointType.BEFORE_TOOL, tool_name="write_file")
        ])
        result = config.should_break(BreakpointType.BEFORE_TOOL, tool_name="read_file")
        assert result is None

    def test_no_tool_filter_matches_any_tool(self) -> None:
        config = BreakpointsConfig(breakpoints=[
            BreakpointConfig(bp_type=BreakpointType.BEFORE_TOOL)
        ])
        result = config.should_break(BreakpointType.BEFORE_TOOL, tool_name="any_tool")
        assert result is not None

    def test_first_match_wins(self) -> None:
        config = BreakpointsConfig(breakpoints=[
            BreakpointConfig(bp_type=BreakpointType.BEFORE_TOOL, tool_name="write_file"),
            BreakpointConfig(bp_type=BreakpointType.BEFORE_TOOL),
        ])
        result = config.should_break(BreakpointType.BEFORE_TOOL, tool_name="write_file")
        assert result is not None
        assert result.tool_name == "write_file"


class TestIsWriteTool:
    def test_write_file_is_write(self) -> None:
        assert is_write_tool("write_file") is True

    def test_read_file_is_not_write(self) -> None:
        assert is_write_tool("read_file") is False

    def test_python_execute_is_not_write(self) -> None:
        assert is_write_tool("python_execute") is False

    def test_unknown_tool_is_not_write(self) -> None:
        assert is_write_tool("unknown") is False


class TestMergeBreakpoints:
    def test_both_none_returns_empty(self) -> None:
        config = merge_breakpoints(None, None)
        assert config.is_empty

    def test_global_only(self) -> None:
        config = merge_breakpoints([{"type": "before_tool"}], None)
        assert len(config.breakpoints) == 1

    def test_conversation_only(self) -> None:
        config = merge_breakpoints(None, [{"type": "before_write"}])
        assert len(config.breakpoints) == 1

    def test_both_concatenated(self) -> None:
        config = merge_breakpoints(
            [{"type": "before_tool"}],
            [{"type": "before_write"}],
        )
        assert len(config.breakpoints) == 2
        assert config.breakpoints[0].bp_type == BreakpointType.BEFORE_TOOL
        assert config.breakpoints[1].bp_type == BreakpointType.BEFORE_WRITE
