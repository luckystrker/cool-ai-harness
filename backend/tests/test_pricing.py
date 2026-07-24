"""Tests for the pricing table and cost estimation (Фаза 1.5 §5)."""

from __future__ import annotations

from app.providers.pricing import (
    estimate_cost_usd,
    has_pricing,
)


def test_known_model_returns_cost() -> None:
    # gpt-4o-mini: $0.00015/1k prompt, $0.0006/1k completion.
    cost = estimate_cost_usd("gpt-4o-mini", 1000, 500)
    assert cost is not None
    expected = (1000 / 1000) * 0.00015 + (500 / 1000) * 0.0006
    assert abs(cost - expected) < 1e-9


def test_unknown_model_returns_none() -> None:
    assert estimate_cost_usd("totally-made-up-model", 1000, 1000) is None
    assert has_pricing("totally-made-up-model") is False


def test_date_suffix_stripped() -> None:
    """A dated variant resolves to the base model's price."""
    assert estimate_cost_usd("gpt-4o-2024-08-06", 1000, 0) == estimate_cost_usd(
        "gpt-4o", 1000, 0
    )
    assert estimate_cost_usd("claude-3-5-sonnet-20241022", 1000, 0) is not None


def test_case_insensitive() -> None:
    assert estimate_cost_usd("GPT-4O-MINI", 1000, 500) == estimate_cost_usd(
        "gpt-4o-mini", 1000, 500
    )


def test_longest_prefix_wins() -> None:
    """gpt-4o-mini should not resolve to the gpt-4o price."""
    mini = estimate_cost_usd("gpt-4o-mini", 1000, 0)
    base = estimate_cost_usd("gpt-4o", 1000, 0)
    assert mini is not None and base is not None
    assert mini != base  # mini is cheaper


def test_zero_tokens_zero_cost() -> None:
    cost = estimate_cost_usd("gpt-4o", 0, 0)
    assert cost == 0.0


def test_providers_populate_cost_usd() -> None:
    """OpenAIProvider._parse_usage now fills cost_usd for known models."""
    from app.providers.openai import OpenAIProvider

    usage = OpenAIProvider._parse_usage(
        {"prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500},
        model="gpt-4o-mini",
    )
    assert usage is not None
    assert usage.cost_usd is not None
    assert usage.cost_usd > 0


def test_providers_cost_none_for_unpriced() -> None:
    from app.providers.anthropic import AnthropicProvider

    usage = AnthropicProvider._parse_usage(
        {"input_tokens": 100, "output_tokens": 50}, model="mystery-claude"
    )
    assert usage is not None
    assert usage.cost_usd is None
