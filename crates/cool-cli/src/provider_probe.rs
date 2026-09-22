//! Live provider model-list probe (M11 WS1d / Workstream B3).
//!
//! Python listed models with a live provider call (`providers/openai.py`
//! `list_models`, `providers/anthropic.py` `list_models`) enriched from a static
//! pricing table. This module ports that behavior: an OpenAI-compatible
//! `GET {base}/models` or an Anthropic `GET {base}/v1/models`, a 20 s timeout,
//! `NetworkPolicy`-pinned egress, and per-1k-token price annotation. The
//! plaintext key is used in memory only and never persisted or logged.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_app_server::ProviderProbe;
use cool_protocol::ModelInfoRecord;
use cool_security::{NetworkPolicy, SecretKeyring};
use cool_store::LegacyStore;
use serde_json::Value;

/// Per-1k-token USD prices (prompt, completion); mirrors `providers/pricing.py`.
const PRICING: &[(&str, f64, f64)] = &[
    // OpenAI
    ("gpt-4o", 0.0025, 0.01),
    ("gpt-4o-mini", 0.00015, 0.0006),
    ("gpt-4-turbo", 0.01, 0.03),
    ("gpt-4", 0.03, 0.06),
    ("gpt-3.5-turbo", 0.0005, 0.0015),
    ("o1", 0.015, 0.06),
    ("o1-mini", 0.003, 0.012),
    ("o3-mini", 0.0011, 0.0044),
    // Anthropic
    ("claude-3-5-sonnet", 0.003, 0.015),
    ("claude-3-5-haiku", 0.0008, 0.004),
    ("claude-3-opus", 0.015, 0.075),
    ("claude-3-sonnet", 0.003, 0.015),
    ("claude-3-haiku", 0.00025, 0.00125),
    // DeepSeek
    ("deepseek-chat", 0.00027, 0.0011),
    ("deepseek-reasoner", 0.00055, 0.00219),
    // Groq (rough OpenAI-compatible tiers)
    ("llama-3.3-70b", 0.00059, 0.00079),
    ("llama-3.1-70b", 0.00059, 0.00079),
    ("llama-3.1-8b", 0.00005, 0.00008),
];

/// CLI provider probe over the legacy provider store + secret keyring.
pub struct CliProviderProbe {
    store: Option<Arc<LegacyStore>>,
    secrets: Option<Arc<SecretKeyring>>,
}

impl CliProviderProbe {
    pub fn new(store: Option<Arc<LegacyStore>>, secrets: Option<Arc<SecretKeyring>>) -> Self {
        Self { store, secrets }
    }
}

#[async_trait]
impl ProviderProbe for CliProviderProbe {
    async fn list_models(
        &self,
        actor: &str,
        provider_id: i64,
    ) -> Result<Vec<ModelInfoRecord>, String> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| "provider store unavailable".to_owned())?;
        let provider = store
            .get_provider(actor, provider_id)
            .map_err(|_| "provider not found".to_owned())?;
        // A missing/undecryptable key degrades to an empty key (Python
        // `_provider_row_to_llm` leaves it empty), which the probe sends as the
        // local-compatible default.
        let api_key = provider
            .api_key_encrypted
            .as_deref()
            .and_then(|stored| {
                self.secrets
                    .as_ref()
                    .and_then(|secrets| secrets.decrypt(stored).ok())
            })
            .unwrap_or_default();
        let base_url = provider
            .base_url
            .clone()
            .unwrap_or_else(|| default_base_url(&provider.name));
        probe_models(&provider.name, &base_url, &api_key).await
    }

    async fn preview_models(
        &self,
        name: &str,
        base_url: Option<&str>,
        api_key: &str,
    ) -> Result<Vec<ModelInfoRecord>, String> {
        let base_url = base_url
            .map(str::to_owned)
            .unwrap_or_else(|| default_base_url(name));
        probe_models(name, &base_url, api_key).await
    }
}

/// Python `_default_base_url_for`.
pub fn default_base_url(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "openai" => "https://api.openai.com/v1".to_owned(),
        "anthropic" => "https://api.anthropic.com".to_owned(),
        "openrouter" | "open_router" => "https://openrouter.ai/api/v1".to_owned(),
        "deepseek" => "https://api.deepseek.com/v1".to_owned(),
        "groq" => "https://api.groq.com/openai/v1".to_owned(),
        "ollama" | "local" => "http://localhost:11434/v1".to_owned(),
        _ => std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
    }
}

/// Probe a provider's model list, failing closed on a non-2xx response or an
/// egress-policy denial.
pub async fn probe_models(
    name: &str,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<ModelInfoRecord>, String> {
    let base = base_url.trim().trim_end_matches('/');
    let anthropic = name.eq_ignore_ascii_case("anthropic");
    let url = if anthropic {
        format!("{base}/v1/models")
    } else {
        format!("{base}/models")
    };
    let parsed = url::Url::parse(&url).map_err(|error| format!("invalid provider URL: {error}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "provider URL has no host".to_owned())?
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "provider URL has no port".to_owned())?;
    let resolved = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| "provider host lookup timed out".to_owned())?
    .map_err(|error| format!("provider host lookup failed: {error}"))?
    .collect::<Vec<_>>();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    let policy = if loopback {
        NetworkPolicy::new([host.clone()]).loopback_only()
    } else {
        NetworkPolicy::new([host.clone()])
    };
    let pinned = policy
        .pin(parsed.as_str(), resolved.iter().map(|socket| socket.ip()))
        .map_err(|error| format!("provider URL denied: {error}"))?;
    let address = resolved
        .iter()
        .find(|candidate| pinned.addresses.contains(&candidate.ip()))
        .copied()
        .ok_or_else(|| "no pinned provider address".to_owned())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .resolve(&host, address)
        .build()
        .map_err(|error| format!("provider client failed: {error}"))?;
    let mut request = client.get(parsed);
    if anthropic {
        request = request
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    } else {
        // Python OpenAIProvider defaults an empty key to "ollama" (local servers).
        let key = if api_key.is_empty() {
            "ollama"
        } else {
            api_key
        };
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("provider request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "provider returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|_| "provider response was not JSON".to_owned())?;
    // Python: `payload.get("data") if isinstance(payload, dict) else payload`,
    // so a bare top-level array is also accepted.
    let items = match &payload {
        Value::Object(object) => object
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    let mut models = Vec::new();
    for item in &items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let context_window = if anthropic {
            None
        } else {
            extract_context_window(object)
        };
        let (prompt_price, completion_price) = match pricing(id) {
            Some((prompt, completion)) => (Some(prompt), Some(completion)),
            None => (None, None),
        };
        models.push(ModelInfoRecord {
            id: id.to_owned(),
            context_window,
            prompt_price,
            completion_price,
        });
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(models)
}

/// Python `_extract_context_window`.
fn extract_context_window(object: &serde_json::Map<String, Value>) -> Option<i64> {
    for key in ["context_length", "context_window", "max_context", "n_ctx"] {
        if let Some(value) = object.get(key).and_then(Value::as_i64)
            && value > 0
        {
            return Some(value);
        }
    }
    if let Some(top) = object.get("top_provider").and_then(Value::as_object) {
        for key in ["context_length", "context_window"] {
            if let Some(value) = top.get(key).and_then(Value::as_i64)
                && value > 0
            {
                return Some(value);
            }
        }
    }
    None
}

/// Python `pricing._lookup`: exact, then longest prefix match.
fn pricing(model: &str) -> Option<(f64, f64)> {
    let normalized = normalize(model);
    if let Some(entry) = PRICING.iter().find(|(key, _, _)| *key == normalized) {
        return Some((entry.1, entry.2));
    }
    let mut best: Option<&(&str, f64, f64)> = None;
    for entry in PRICING {
        let key = entry.0;
        let matches = normalized == key
            || normalized.starts_with(&format!("{key}-"))
            || key.starts_with(&format!("{normalized}-"));
        if matches
            && best
                .map(|current| key.len() > current.0.len())
                .unwrap_or(true)
        {
            best = Some(entry);
        }
    }
    best.map(|entry| (entry.1, entry.2))
}

/// Python `pricing._normalize`: lowercase and strip a trailing date stamp.
fn normalize(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    if !lower.is_ascii() {
        return lower;
    }
    match strip_date_suffix(&lower) {
        Some(stripped) => stripped.to_owned(),
        None => lower,
    }
}

fn strip_date_suffix(model: &str) -> Option<&str> {
    let bytes = model.as_bytes();
    if model.len() >= 11 {
        let start = model.len() - 11;
        let tail = &bytes[start..];
        if tail[0] == b'-'
            && tail[1..5].iter().all(u8::is_ascii_digit)
            && tail[5] == b'-'
            && tail[6..8].iter().all(u8::is_ascii_digit)
            && tail[8] == b'-'
            && tail[9..11].iter().all(u8::is_ascii_digit)
        {
            return Some(&model[..start]);
        }
    }
    if model.len() >= 9 {
        let start = model.len() - 9;
        let tail = &bytes[start..];
        if tail[0] == b'-' && tail[1..].iter().all(u8::is_ascii_digit) {
            return Some(&model[..start]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pricing_matches_exact_and_prefix() {
        assert_eq!(pricing("gpt-4o"), Some((0.0025, 0.01)));
        // A date-stamped id resolves to its base entry.
        assert_eq!(pricing("gpt-4o-2024-08-06"), Some((0.0025, 0.01)));
        assert_eq!(pricing("claude-3-5-sonnet-20241022"), Some((0.003, 0.015)));
        // The longest prefix wins ("gpt-4o-mini" beats "gpt-4o").
        assert_eq!(pricing("gpt-4o-mini"), Some((0.00015, 0.0006)));
        assert_eq!(pricing("unknown-model"), None);
    }

    #[test]
    fn default_base_urls_match_python() {
        assert_eq!(default_base_url("anthropic"), "https://api.anthropic.com");
        assert_eq!(default_base_url("ollama"), "http://localhost:11434/v1");
        assert_eq!(
            default_base_url("openrouter"),
            "https://openrouter.ai/api/v1"
        );
    }

    #[test]
    fn context_window_reads_known_keys() {
        let object: serde_json::Map<String, Value> =
            serde_json::from_value(serde_json::json!({"context_length": 128000})).unwrap();
        assert_eq!(extract_context_window(&object), Some(128000));
        let top: serde_json::Map<String, Value> =
            serde_json::from_value(serde_json::json!({"top_provider": {"context_window": 8192}}))
                .unwrap();
        assert_eq!(extract_context_window(&top), Some(8192));
        assert_eq!(extract_context_window(&serde_json::Map::new()), None);
    }

    #[tokio::test]
    async fn probe_denies_a_private_non_loopback_host() {
        // 10.0.0.1 is private, so the egress policy must deny it before any call.
        let error = probe_models("openai", "http://10.0.0.1:8080/v1", "key")
            .await
            .expect_err("a private host must be denied");
        assert!(error.contains("denied"), "unexpected error: {error}");
    }
}
