//! Model pricing table and cost estimation.
//!
//! Parity with `backend/app/providers/pricing.py` (Фаза 1.5 §5): providers
//! report token usage but never a dollar cost, so this module maps a model
//! name to per-1k-token prices and estimates the cost of a completion. Unknown
//! models return `None` so callers (and the per-run cost guard) stay
//! fail-inert on unpriced traffic.
//!
//! Prices are approximations sourced from public provider pricing pages; they
//! are not authoritative. The goal is budget guardrails, not billing-grade
//! accuracy. The table keeps every Python entry and adds the current
//! generation (`gpt-5*`, `claude-*-4*`) so the CLI's default model is priced
//! too. Values are integer micro-USD per 1k tokens; the smallest Python entry
//! (50 µ$/1k) stays exact.

/// One pricing entry: micro-USD charged per 1k prompt / completion tokens.
/// `cache_*` columns price Anthropic prompt caching; zero means the model has
/// no published cache price — cache usage on such a model stays fail-closed
/// (cost reports as unknown rather than silently free).
struct ModelPricing {
    prompt_per_1k_micro_usd: u64,
    completion_per_1k_micro_usd: u64,
    cache_read_per_1k_micro_usd: u64,
    cache_write_per_1k_micro_usd: u64,
}

const fn pricing(prompt: u64, completion: u64) -> ModelPricing {
    ModelPricing {
        prompt_per_1k_micro_usd: prompt,
        completion_per_1k_micro_usd: completion,
        cache_read_per_1k_micro_usd: 0,
        cache_write_per_1k_micro_usd: 0,
    }
}

/// Anthropic prompt-caching prices derive from the base input price: a cache
/// write costs 1.25x input (5-minute TTL) and a cache read 0.1x input.
const fn pricing_cached(prompt: u64, completion: u64) -> ModelPricing {
    ModelPricing {
        prompt_per_1k_micro_usd: prompt,
        completion_per_1k_micro_usd: completion,
        cache_read_per_1k_micro_usd: prompt / 10,
        cache_write_per_1k_micro_usd: prompt * 5 / 4,
    }
}

// Order matters only for readability; lookup picks the longest prefix match.
static PRICING: &[(&str, ModelPricing)] = &[
    // --- OpenAI ---
    ("gpt-4o", pricing(2_500, 10_000)),
    ("gpt-4o-mini", pricing(150, 600)),
    ("gpt-4-turbo", pricing(10_000, 30_000)),
    ("gpt-4", pricing(30_000, 60_000)),
    ("gpt-3.5-turbo", pricing(500, 1_500)),
    ("o1", pricing(15_000, 60_000)),
    ("o1-mini", pricing(3_000, 12_000)),
    ("o3-mini", pricing(1_100, 4_400)),
    ("gpt-5", pricing(1_250, 10_000)),
    ("gpt-5-mini", pricing(250, 2_000)),
    ("gpt-5-nano", pricing(50, 400)),
    // --- Anthropic (cache columns: read 0.1x, write 1.25x the input price) ---
    ("claude-3-5-sonnet", pricing_cached(3_000, 15_000)),
    ("claude-3-5-haiku", pricing_cached(800, 4_000)),
    ("claude-3-opus", pricing_cached(15_000, 75_000)),
    ("claude-3-sonnet", pricing_cached(3_000, 15_000)),
    ("claude-3-haiku", pricing_cached(250, 1_250)),
    ("claude-sonnet-4", pricing_cached(3_000, 15_000)),
    ("claude-opus-4", pricing_cached(15_000, 75_000)),
    ("claude-haiku-4", pricing_cached(1_000, 5_000)),
    // --- Google Gemini (AI Studio paid tier) ---
    ("gemini-2.5-pro", pricing(1_250, 10_000)),
    ("gemini-2.5-flash", pricing(300, 2_500)),
    ("gemini-2.0-flash", pricing(100, 400)),
    // --- DeepSeek ---
    ("deepseek-chat", pricing(270, 1_100)),
    ("deepseek-reasoner", pricing(550, 2_190)),
    // --- Groq (open models; rough OpenAI-compatible tiers) ---
    ("llama-3.3-70b", pricing(590, 790)),
    ("llama-3.1-70b", pricing(590, 790)),
    ("llama-3.1-8b", pricing(50, 80)),
];

/// Lowercase and strip a trailing date/revision stamp for matching, mirroring
/// Python `_normalize`: `"gpt-4o-2024-08-06"` → `"gpt-4o"`,
/// `"claude-sonnet-4-5-20250929"` → `"claude-sonnet-4-5"`.
fn normalize(model: &str) -> String {
    let mut normalized = model.trim().to_lowercase();
    let mut bytes = normalized.as_bytes();
    for suffix_len in [11_usize, 9_usize] {
        // "-YYYY-MM-DD" (11 bytes) or "-YYYYMMDD" (9 bytes).
        if bytes.len() <= suffix_len {
            continue;
        }
        let tail = &bytes[bytes.len() - suffix_len..];
        let matches = if suffix_len == 11 {
            tail[0] == b'-'
                && tail[5] == b'-'
                && tail[8] == b'-'
                && tail[1..5].iter().all(u8::is_ascii_digit)
                && tail[6..8].iter().all(u8::is_ascii_digit)
                && tail[9..].iter().all(u8::is_ascii_digit)
        } else {
            tail[0] == b'-' && tail[1..].iter().all(u8::is_ascii_digit)
        };
        if matches {
            // Safe boundary: the matched tail is pure ASCII.
            normalized.truncate(normalized.len() - suffix_len);
            bytes = normalized.as_bytes();
        }
    }
    normalized
}

/// Exact match first, then the longest prefix match so `"gpt-4o-mini"` beats
/// `"gpt-4o"` — mirrors Python `_lookup`.
fn lookup(model: &str) -> Option<&'static ModelPricing> {
    let normalized = normalize(model);
    if let Some((_, entry)) = PRICING.iter().find(|(key, _)| *key == normalized) {
        return Some(entry);
    }
    PRICING
        .iter()
        .filter(|(key, _)| {
            normalized == *key
                || normalized.starts_with(&format!("{key}-"))
                || key.starts_with(&format!("{normalized}-"))
        })
        .max_by_key(|(key, _)| key.len())
        .map(|(_, entry)| entry)
}

/// Estimate the cost of a completion in micro-USD (1 USD = 1_000_000 µ$).
/// `cache_read_tokens`/`cache_write_tokens` are Anthropic prompt-caching
/// counters; pass 0 for providers that do not report them.
///
/// Returns `None` when the model is unknown — or when it reports cache usage
/// but has no published cache prices — so the caller leaves `cost_micro_usd`
/// unset and the run-budget guard stays fail-closed for unpriced traffic.
pub fn estimate_cost_micro_usd(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> Option<u64> {
    let prices = lookup(model)?;
    if (cache_read_tokens > 0 || cache_write_tokens > 0)
        && (prices.cache_read_per_1k_micro_usd == 0 || prices.cache_write_per_1k_micro_usd == 0)
    {
        return None;
    }
    let prompt = u128::from(prompt_tokens) * u128::from(prices.prompt_per_1k_micro_usd);
    let completion = u128::from(completion_tokens) * u128::from(prices.completion_per_1k_micro_usd);
    let cache_read = u128::from(cache_read_tokens) * u128::from(prices.cache_read_per_1k_micro_usd);
    let cache_write =
        u128::from(cache_write_tokens) * u128::from(prices.cache_write_per_1k_micro_usd);
    Some(((prompt + completion + cache_read + cache_write + 500) / 1_000) as u64)
}

/// Whether `estimate_cost_micro_usd` returns a value for `model`.
pub fn has_pricing(model: &str) -> bool {
    lookup(model).is_some()
}

/// Per-1k-token micro-USD prices for `model` — `(prompt, completion,
/// cache_read, cache_write)` — used to annotate model lists (the provider
/// settings probe mirrors Python `list_models`).
pub fn model_pricing(model: &str) -> Option<(u64, u64, u64, u64)> {
    lookup(model).map(|entry| {
        (
            entry.prompt_per_1k_micro_usd,
            entry.completion_per_1k_micro_usd,
            entry.cache_read_per_1k_micro_usd,
            entry.cache_write_per_1k_micro_usd,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_prefix_lookup_match_python() {
        assert_eq!(
            estimate_cost_micro_usd("gpt-4o", 1_000, 1_000, 0, 0),
            Some(12_500)
        );
        assert_eq!(
            estimate_cost_micro_usd("gpt-4o-2024-08-06", 1_000, 0, 0, 0),
            Some(2_500)
        );
        assert_eq!(
            estimate_cost_micro_usd("gpt-4o-mini-2024-07-18", 0, 1_000, 0, 0),
            Some(600)
        );
        // Longest prefix wins: "gpt-4o-mini-foo" prices at mini, not 4o.
        assert_eq!(
            estimate_cost_micro_usd("gpt-4o-mini-preview", 1_000, 0, 0, 0),
            Some(150)
        );
        assert_eq!(
            estimate_cost_micro_usd("claude-sonnet-4-5-20250929", 1_000, 1_000, 0, 0),
            Some(18_000)
        );
        assert_eq!(estimate_cost_micro_usd("unknown-model", 1, 1, 0, 0), None);
    }

    #[test]
    fn cache_tokens_price_at_anthropic_multipliers() {
        // claude-sonnet-4: 3000/1k input -> 300/1k read, 3750/1k write.
        assert_eq!(
            estimate_cost_micro_usd("claude-sonnet-4-5", 1_000, 0, 2_000, 1_000),
            Some(3_000 + 600 + 3_750)
        );
        // Cache usage on a model without cache prices is fail-closed.
        assert_eq!(estimate_cost_micro_usd("gpt-4o", 0, 0, 1, 0), None);
    }

    #[test]
    fn normalize_strips_both_date_shapes() {
        assert_eq!(normalize("GPT-4O-2024-08-06"), "gpt-4o");
        assert_eq!(normalize("claude-3-5-sonnet-20241022"), "claude-3-5-sonnet");
        assert_eq!(normalize("deepseek-chat"), "deepseek-chat");
        // A trailing non-date number is not stripped.
        assert_eq!(normalize("gpt-3.5-turbo-0125"), "gpt-3.5-turbo-0125");
    }

    #[test]
    fn lookup_keeps_partial_names_safe() {
        // "gpt-4" is a key; "gpt-4-1106-preview" should still land on it.
        assert!(has_pricing("gpt-4-1106-preview"));
        assert!(!has_pricing("gpt-99"));
    }
}
