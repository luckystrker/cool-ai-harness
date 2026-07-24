"""Model pricing table and cost estimation (Фаза 1.5 §5).

Providers report token usage but never a dollar cost. This module maps a model
name to per-1k-token prices (USD) and computes the cost of a completion. Returns
``None`` for unknown models so callers (and the existing per-run cost guard)
stay inert when pricing is unavailable.

Prices are approximations sourced from public provider pricing pages; they are
not authoritative. The goal is budget guardrails, not billing-grade accuracy.
"""

from __future__ import annotations

from app.core.logging import get_logger

log = get_logger(__name__)

# Per-1k-token USD prices: {"prompt": float, "completion": float}.
# Keys are matched against ``model`` case-insensitively by prefix/segment match
# (see _lookup) so e.g. "gpt-4o-2024-08-06" resolves to the "gpt-4o" entry.
_PRICING: dict[str, dict[str, float]] = {
    # --- OpenAI ---
    "gpt-4o": {"prompt": 0.0025, "completion": 0.01},
    "gpt-4o-mini": {"prompt": 0.00015, "completion": 0.0006},
    "gpt-4-turbo": {"prompt": 0.01, "completion": 0.03},
    "gpt-4": {"prompt": 0.03, "completion": 0.06},
    "gpt-3.5-turbo": {"prompt": 0.0005, "completion": 0.0015},
    "o1": {"prompt": 0.015, "completion": 0.06},
    "o1-mini": {"prompt": 0.003, "completion": 0.012},
    "o3-mini": {"prompt": 0.0011, "completion": 0.0044},
    # --- Anthropic ---
    "claude-3-5-sonnet": {"prompt": 0.003, "completion": 0.015},
    "claude-3-5-haiku": {"prompt": 0.0008, "completion": 0.004},
    "claude-3-opus": {"prompt": 0.015, "completion": 0.075},
    "claude-3-sonnet": {"prompt": 0.003, "completion": 0.015},
    "claude-3-haiku": {"prompt": 0.00025, "completion": 0.00125},
    # --- DeepSeek ---
    "deepseek-chat": {"prompt": 0.00027, "completion": 0.0011},
    "deepseek-reasoner": {"prompt": 0.00055, "completion": 0.00219},
    # --- Groq (open models; rough OpenAI-compatible tiers) ---
    "llama-3.3-70b": {"prompt": 0.00059, "completion": 0.00079},
    "llama-3.1-70b": {"prompt": 0.00059, "completion": 0.00079},
    "llama-3.1-8b": {"prompt": 0.00005, "completion": 0.00008},
}


def _normalize(model: str) -> str:
    """Lowercase and strip a date/revision suffix for matching.

    "gpt-4o-2024-08-06"   -> "gpt-4o"
    "claude-3-5-sonnet-20241022" -> "claude-3-5-sonnet"
    "deepseek-chat"       -> "deepseek-chat" (unchanged)
    """
    m = model.strip().lower()
    # Strip a trailing "-YYYYMMDD" or "-YYYY-MM-DD" date stamp.
    import re

    m = re.sub(r"-\d{4}-\d{2}-\d{2}$", "", m)
    m = re.sub(r"-\d{8}$", "", m)
    return m


def _lookup(model: str) -> dict[str, float] | None:
    """Find a pricing entry for ``model`` by exact, then prefix, match."""
    norm = _normalize(model)
    if norm in _PRICING:
        return _PRICING[norm]
    # Prefer the longest matching prefix (so "gpt-4o-mini" beats "gpt-4o").
    best_key: str | None = None
    for key in _PRICING:
        if norm == key or norm.startswith(key + "-") or key.startswith(norm + "-"):
            if best_key is None or len(key) > len(best_key):
                best_key = key
    return _PRICING[best_key] if best_key else None


def estimate_cost_usd(
    model: str, prompt_tokens: int, completion_tokens: int
) -> float | None:
    """Estimate the USD cost of a completion.

    Returns ``None`` when the model is unknown (so callers can stay inert).
    """
    prices = _lookup(model)
    if prices is None:
        return None
    cost = (prompt_tokens / 1000.0) * prices["prompt"] + (
        completion_tokens / 1000.0
    ) * prices["completion"]
    return round(cost, 6)


def has_pricing(model: str) -> bool:
    """Whether ``estimate_cost_usd`` will return a non-None value for ``model``."""
    return _lookup(model) is not None


def get_model_pricing(model: str) -> dict[str, float] | None:
    """Return the ``{"prompt": float, "completion": float}`` entry for ``model``.

    Public accessor over the private prefix-matching lookup, used to annotate
    model lists (e.g. the provider settings UI) with per-1k-token prices.
    Returns ``None`` for unknown models.
    """
    return _lookup(model)
