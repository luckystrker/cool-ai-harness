//! `web_search` / `web_fetch` tools — Python `tools/web_tools.py` parity (M12).
//!
//! `web_fetch` is pinned-egress: every hop resolves once, pins the allowed
//! addresses into the HTTP client, and re-validates redirects (same machinery
//! as `cool-cli/src/rss_feed.rs`). Deliberate hardening vs Python:
//!
//! - DNS answers are pinned into the connection (`resolve()`), closing the
//!   rebind window Python leaves open between check and fetch.
//! - Non-loopback private addresses are always denied; Python's
//!   `SSRF_BLOCK_PRIVATE_IPS=false` escape hatch is *not* ported — it is a
//!   security-invariant reduction (it also permits link-local/metadata
//!   endpoints). Loopback stays reachable only for an explicitly configured
//!   `SEARXNG_URL` pointing at loopback.
//! - POST redirects are not followed (the search-provider calls do not
//!   legitimately redirect); Python/httpx would rewrite 301/302 to GET.
//!
//! `web_search` is provider-pluggable like Python: `SEARCH_PROVIDER` selects
//! `serper` | `tavily` | `searxng`; an unset provider answers a graceful
//! "no provider configured" tool error rather than failing the run.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use cool_security::{Capability, Decision, NetworkPolicy, mask_secrets};
use regex::Regex;
use serde_json::{Value, json};
use url::Url;

use crate::tools::{
    Tool, ToolContext, ToolDefinition, ToolError, ToolHandler, ToolResult,
};

const MAX_REDIRECTS: u8 = 10; // Python `_MAX_REDIRECTS`
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 500_000; // Python `network_max_response_bytes`
const HARD_MAX_RESPONSE_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_MAX_CHARS: usize = 20_000;
const MAX_NUM_RESULTS: i64 = 20;
const DEFAULT_NUM_RESULTS: i64 = 5;
const MAX_QUERY_CHARS: usize = 2_000;
const SNIPPET_CHARS: usize = 500;
const TITLE_CHARS: usize = 200;
const USER_AGENT: &str = "CoolAIHarness/0.1";

/// Server-side configuration for the web tools (env-driven, like Python's
/// `get_settings()` fields).
#[derive(Clone, Debug, Default)]
pub struct WebToolsConfig {
    /// `serper` | `tavily` | `searxng` (empty = graceful "not configured").
    pub search_provider: String,
    pub serper_api_key: String,
    pub tavily_api_key: String,
    /// Base URL of a self-hosted SearXNG instance (loopback allowed).
    pub searxng_url: String,
    /// Domain allowlist for `web_fetch` (empty = all public domains).
    pub allowed_domains: Vec<String>,
    /// Response body cap for `web_fetch` (0 = the hard cap).
    pub max_response_bytes: u64,
}

impl WebToolsConfig {
    /// Read the operator environment. `NETWORK_ALLOWED_DOMAINS` accepts either
    /// a JSON array (pydantic-settings convention) or a comma-separated list
    /// (the documented `.env.example` shape).
    pub fn from_env() -> Self {
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        Self {
            search_provider: env("SEARCH_PROVIDER").to_lowercase(),
            serper_api_key: env("SERPER_API_KEY"),
            tavily_api_key: env("TAVILY_API_KEY"),
            searxng_url: env("SEARXNG_URL"),
            allowed_domains: parse_allowed_domains(&env("NETWORK_ALLOWED_DOMAINS")),
            max_response_bytes: env("NETWORK_MAX_RESPONSE_BYTES")
                .parse::<u64>()
                .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES),
        }
    }

    fn fetch_policy(&self) -> NetworkPolicy {
        let mut policy = NetworkPolicy::new(self.allowed_domains.clone());
        policy.max_redirects = MAX_REDIRECTS;
        policy.max_response_bytes = HARD_MAX_RESPONSE_BYTES;
        policy.timeout = FETCH_TIMEOUT;
        policy
    }
}

fn parse_allowed_domains(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if let Ok(list) = serde_json::from_str::<Vec<String>>(raw) {
        return list
            .into_iter()
            .map(|domain| domain.trim().to_ascii_lowercase())
            .filter(|domain| !domain.is_empty())
            .collect();
    }
    raw.split(',')
        .map(|domain| domain.trim().to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
        .collect()
}

/// Build the two web tools; register onto any registry via `extend`.
pub fn web_tool_registry(config: WebToolsConfig) -> Vec<Tool> {
    let config = Arc::new(config);
    vec![
        Tool::new(
            ToolDefinition {
                name: "web_search".to_owned(),
                description: "Search the web for a query and return the top results (title, URL, snippet). Provider is configured server-side.".to_owned(),
                parameters: json!({"type":"object","properties":{"query":{"type":"string"},"num_results":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false}),
            },
            [Capability::Network],
            Decision::Allow,
            WebSearch { config: config.clone() },
        ),
        Tool::new(
            ToolDefinition {
                name: "web_fetch".to_owned(),
                description: "Download a URL and return its main text content with HTML tags stripped. SSRF-protected: private IPs are blocked and a domain allowlist may be configured. Useful for reading an article returned by web_search.".to_owned(),
                parameters: json!({"type":"object","properties":{"url":{"type":"string"},"max_chars":{"type":"integer","minimum":1,"maximum":200000}},"required":["url"],"additionalProperties":false}),
            },
            [Capability::Network],
            Decision::Allow,
            WebFetch { config },
        ),
    ]
}

// --- pinned egress ---------------------------------------------------------

/// Resolve `host:port` once (with the fetch timeout) and return the IPs.
async fn resolve_ips(url: &Url, timeout: Duration) -> Result<Vec<IpAddr>, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_owned())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL has no port".to_owned())?;
    Ok(tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| "host lookup timed out".to_owned())?
        .map_err(|error| format!("host lookup failed: {error}"))?
        .map(|socket| socket.ip())
        .collect())
}

struct PinnedResponse {
    status: u16,
    final_url: Url,
    body: Vec<u8>,
    /// Body hit the byte cap mid-download (Python `size_truncated`).
    size_truncated: bool,
}

/// GET with per-hop re-pinning. Redirects re-resolve and re-validate through
/// `policy`; exceeding `policy.max_redirects` fails.
async fn pinned_get(
    url: &Url,
    policy: &NetworkPolicy,
    max_bytes: u64,
) -> Result<PinnedResponse, String> {
    let mut current = url.clone();
    let mut hops = 0u8;
    loop {
        match pinned_request(&current, policy, None, max_bytes).await? {
            PinnedOutcome::Response(response) => return Ok(response),
            PinnedOutcome::Redirect(location) => {
                hops += 1;
                if hops > policy.max_redirects {
                    return Err(format!("too many redirects (>{})", policy.max_redirects));
                }
                current = current
                    .join(&location)
                    .map_err(|error| format!("invalid redirect: {error}"))?;
            }
        }
    }
}

/// POST with a JSON body (search-provider APIs). Redirects are rejected —
/// none of the configured providers legitimately redirect.
async fn pinned_post_json(
    url: &Url,
    policy: &NetworkPolicy,
    body: Vec<u8>,
    extra_headers: &[(String, String)],
) -> Result<PinnedResponse, String> {
    match pinned_request(url, policy, Some((body, extra_headers)), HARD_MAX_RESPONSE_BYTES).await? {
        PinnedOutcome::Response(response) => Ok(response),
        PinnedOutcome::Redirect(_) => Err("redirect on a search POST is not followed".to_owned()),
    }
}

enum PinnedOutcome {
    Response(PinnedResponse),
    Redirect(String),
}

/// One pinned request: resolve, validate against `policy`, pin the socket
/// address into a one-shot client, cap the body mid-stream.
async fn pinned_request(
    url: &Url,
    policy: &NetworkPolicy,
    post: Option<(Vec<u8>, &[(String, String)])>,
    max_bytes: u64,
) -> Result<PinnedOutcome, String> {
    let resolved = resolve_ips(url, policy.timeout).await?;
    let pinned = policy
        .pin(url.as_str(), resolved.iter().copied())
        .map_err(|error| format!("URL denied: {error}"))?;
    // IPv4 first for reachability; only pinned (policy-checked) addresses.
    let ip = resolved
        .iter()
        .copied()
        .find(|ip| ip.is_ipv4() && pinned.addresses.contains(ip))
        .or_else(|| {
            resolved
                .iter()
                .copied()
                .find(|ip| pinned.addresses.contains(ip))
        })
        .ok_or_else(|| "no allowed address after pinning".to_owned())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL has no port".to_owned())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(policy.timeout)
        .no_proxy()
        .resolve(&pinned.host, std::net::SocketAddr::new(ip, port))
        .build()
        .map_err(|error| format!("http client failed: {error}"))?;
    let request = match &post {
        Some((body, headers)) => {
            let mut request = client
                .post(url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            for (name, value) in *headers {
                request = request.header(name.as_str(), value.as_str());
            }
            request
        }
        None => client
            .get(url.clone())
            .header(reqwest::header::USER_AGENT, USER_AGENT),
    };
    let response = request
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| "redirect with no Location header".to_owned())?;
        return Ok(PinnedOutcome::Redirect(location.to_owned()));
    }
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let cap = if max_bytes == 0 {
        HARD_MAX_RESPONSE_BYTES
    } else {
        max_bytes.min(HARD_MAX_RESPONSE_BYTES)
    } as usize;
    let mut response = response;
    let mut body: Vec<u8> = Vec::new();
    let mut size_truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("body read failed: {error}"))?
    {
        if body.len() + chunk.len() > cap {
            let remaining = cap.saturating_sub(body.len());
            body.extend_from_slice(&chunk[..remaining]);
            size_truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(PinnedOutcome::Response(PinnedResponse {
        status: status.as_u16(),
        final_url: url.clone(),
        body,
        size_truncated,
    }))
}

// --- web_search ------------------------------------------------------------

struct WebSearch {
    config: Arc<WebToolsConfig>,
}

#[async_trait]
impl ToolHandler for WebSearch {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments("query must be a non-empty string".to_owned()))?;
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(ToolError::InvalidArguments(format!(
                "query exceeds {MAX_QUERY_CHARS} chars"
            )));
        }
        let num_results = arguments
            .get("num_results")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_NUM_RESULTS)
            .clamp(1, MAX_NUM_RESULTS);
        match self.config.search_provider.as_str() {
            "serper" => self.serper(query, num_results).await,
            "tavily" => self.tavily(query, num_results).await,
            "searxng" => self.searxng(query, num_results).await,
            _ => Ok(ToolResult::error(
                "search_provider_unconfigured",
                "No web search provider configured. Set SEARCH_PROVIDER=serper|tavily|searxng and the matching API key / URL in your .env.",
            )),
        }
    }
}

impl WebSearch {
    /// Fixed public API endpoints still go through pinned egress.
    async fn serper(&self, query: &str, num_results: i64) -> Result<ToolResult, ToolError> {
        if self.config.serper_api_key.is_empty() {
            return Ok(ToolResult::error(
                "search_provider_unconfigured",
                "SEARCH_PROVIDER=serper but SERPER_API_KEY is empty.",
            ));
        }
        let url = Url::parse("https://google.serper.dev/search").unwrap();
        let policy = NetworkPolicy::new(["google.serper.dev".to_owned()]);
        let body = json!({"q": query, "num": num_results}).to_string().into_bytes();
        let headers = [("X-API-KEY".to_owned(), self.config.serper_api_key.clone())];
        let response = match pinned_post_json(&url, &policy, body, &headers).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(ToolResult::error("search_failed", mask_secrets(&error)));
            }
        };
        let data: Value = match serde_json::from_slice(&response.body) {
            Ok(data) => data,
            Err(error) => {
                return Ok(ToolResult::error(
                    "search_failed",
                    format!("bad serper JSON: {error}"),
                ));
            }
        };
        let items = data
            .get("organic")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(format_results(
            query,
            items.iter().take(num_results as usize).map(|item| {
                (
                    item.get("title").and_then(Value::as_str).unwrap_or(""),
                    item.get("link").and_then(Value::as_str).unwrap_or(""),
                    item.get("snippet").and_then(Value::as_str).unwrap_or(""),
                )
            }),
        ))
    }

    async fn tavily(&self, query: &str, num_results: i64) -> Result<ToolResult, ToolError> {
        if self.config.tavily_api_key.is_empty() {
            return Ok(ToolResult::error(
                "search_provider_unconfigured",
                "SEARCH_PROVIDER=tavily but TAVILY_API_KEY is empty.",
            ));
        }
        let url = Url::parse("https://api.tavily.com/search").unwrap();
        let policy = NetworkPolicy::new(["api.tavily.com".to_owned()]);
        let body = json!({"api_key": self.config.tavily_api_key, "query": query, "max_results": num_results})
            .to_string()
            .into_bytes();
        let response = match pinned_post_json(&url, &policy, body, &[]).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(ToolResult::error("search_failed", mask_secrets(&error)));
            }
        };
        let data: Value = match serde_json::from_slice(&response.body) {
            Ok(data) => data,
            Err(error) => {
                return Ok(ToolResult::error(
                    "search_failed",
                    format!("bad tavily JSON: {error}"),
                ));
            }
        };
        let items = data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(format_results(
            query,
            items.iter().take(num_results as usize).map(|item| {
                (
                    item.get("title").and_then(Value::as_str).unwrap_or(""),
                    item.get("url").and_then(Value::as_str).unwrap_or(""),
                    item.get("content").and_then(Value::as_str).unwrap_or(""),
                )
            }),
        ))
    }

    async fn searxng(&self, query: &str, num_results: i64) -> Result<ToolResult, ToolError> {
        if self.config.searxng_url.is_empty() {
            return Ok(ToolResult::error(
                "search_provider_unconfigured",
                "SEARCH_PROVIDER=searxng but SEARXNG_URL is empty.",
            ));
        }
        let base = Url::parse(self.config.searxng_url.trim_end_matches('/'))
            .map_err(|error| ToolError::InvalidArguments(format!("SEARXNG_URL invalid: {error}")))?;
        let mut url = base
            .join("search")
            .map_err(|error| ToolError::InvalidArguments(format!("SEARXNG_URL invalid: {error}")))?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "json");
        // A self-hosted SearXNG on loopback is the documented deployment; pin
        // the policy to the configured host and permit loopback only for it.
        let host = url.host_str().unwrap_or_default().to_owned();
        let loopback = is_loopback_host(&host);
        let mut policy = NetworkPolicy::new([host]);
        policy.timeout = SEARCH_TIMEOUT;
        if loopback {
            policy = policy.loopback_only();
        }
        let response = match pinned_get(&url, &policy, HARD_MAX_RESPONSE_BYTES).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(ToolResult::error("search_failed", mask_secrets(&error)));
            }
        };
        let data: Value = match serde_json::from_slice(&response.body) {
            Ok(data) => data,
            Err(error) => {
                return Ok(ToolResult::error(
                    "search_failed",
                    format!("bad searxng JSON: {error}"),
                ));
            }
        };
        let items = data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(format_results(
            query,
            items.iter().take(num_results as usize).map(|item| {
                (
                    item.get("title").and_then(Value::as_str).unwrap_or(""),
                    item.get("url").and_then(Value::as_str).unwrap_or(""),
                    item.get("content").and_then(Value::as_str).unwrap_or(""),
                )
            }),
        ))
    }
}

fn format_results<'a>(
    query: &str,
    items: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> ToolResult {
    let items: Vec<_> = items.collect();
    if items.is_empty() {
        return ToolResult::ok(json!({"text": format!("No results for: {query}"), "count": 0}));
    }
    let mut lines = vec![format!("# Web search: {query}"), format!("{} result(s)\n", items.len())];
    for (index, (title, link, snippet)) in items.iter().enumerate() {
        let title: String = title.chars().take(TITLE_CHARS).collect();
        let snippet: String = snippet.chars().take(SNIPPET_CHARS).collect();
        let title = if title.is_empty() { "(untitled)" } else { title.as_str() };
        lines.push(format!("## {}. {title}\nURL: {link}\n{snippet}\n", index + 1));
    }
    ToolResult::ok(json!({"text": lines.join("\n"), "count": items.len()}))
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

// --- web_fetch -------------------------------------------------------------

struct WebFetch {
    config: Arc<WebToolsConfig>,
}

fn html_strip() -> &'static (Regex, Regex, Regex) {
    static PATTERNS: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    // Two separate patterns: Rust regex has no backreferences, so the Python
    // `<(script|style)...</\1>` is split per tag name.
    PATTERNS.get_or_init(|| {
        (
            Regex::new(r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>").unwrap(),
            Regex::new(r"<[^>]+>").unwrap(),
            Regex::new(r"\s+").unwrap(),
        )
    })
}

#[async_trait]
impl ToolHandler for WebFetch {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        let url = arguments
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments("url must be a non-empty string".to_owned()))?;
        let max_chars = arguments
            .get("max_chars")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_CHARS as u64)
            .clamp(1, 200_000) as usize;
        let url = Url::parse(url)
            .map_err(|error| ToolError::InvalidArguments(format!("invalid url: {error}")))?;
        let response = match pinned_get(&url, &self.config.fetch_policy(), self.config.max_response_bytes)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return Ok(ToolResult::error(
                    "fetch_denied",
                    format!("URL blocked (SSRF protection): {}", mask_secrets(&error)),
                ));
            }
        };
        let body = String::from_utf8_lossy(&response.body);
        let (script, tag, whitespace) = html_strip();
        let text = script.replace_all(&body, " ");
        let text = tag.replace_all(&text, " ");
        let text = whitespace.replace_all(&text, " ").trim().to_owned();
        let mut truncated = response.size_truncated;
        let mut text = text;
        if text.chars().count() > max_chars {
            text = text.chars().take(max_chars).collect();
            text.push_str(&format!("\n[... truncated at {max_chars} chars]"));
            truncated = true;
        }
        if response.size_truncated {
            text.push_str(&format!(
                "\n[... response body truncated at {} bytes]",
                self.config.max_response_bytes
            ));
        }
        let text = mask_secrets(if text.is_empty() { "(empty body)" } else { &text });
        Ok(ToolResult::ok(json!({
            "text": text,
            "final_url": response.final_url.as_str(),
            "status_code": response.status,
            "truncated": truncated,
            "size_truncated": response.size_truncated,
        })))
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::ToolRegistry;
    use cool_security::{CapabilityPolicy, Workspace};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    fn context() -> ToolContext {
        ToolContext::new(
            Workspace::new(std::env::temp_dir()).unwrap(),
            CapabilityPolicy::new(Some(Decision::Allow)),
        )
    }

    fn tool(name: &str, config: WebToolsConfig) -> Tool {
        ToolRegistry::new(web_tool_registry(config))
            .unwrap()
            .get(name)
            .unwrap()
    }

    #[tokio::test]
    async fn web_search_without_provider_errors_gracefully() {
        let tool = tool("web_search", WebToolsConfig::default());
        let result = tool
            .execute(&context(), json!({"query": "rust"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output["error"]
            .as_str()
            .unwrap()
            .contains("SEARCH_PROVIDER"));
    }

    #[tokio::test]
    async fn web_search_provider_without_key_errors_gracefully() {
        let tool = tool(
            "web_search",
            WebToolsConfig {
                search_provider: "serper".to_owned(),
                ..WebToolsConfig::default()
            },
        );
        let result = tool
            .execute(&context(), json!({"query": "rust"}))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output["error"].as_str().unwrap().contains("SERPER_API_KEY"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_private_ips() {
        let tool = tool("web_fetch", WebToolsConfig::default());
        for url in [
            "http://10.0.0.1/",
            "http://127.0.0.1:8080/",
            "http://192.168.1.1/",
            "http://localhost/admin",
        ] {
            let result = tool.execute(&context(), json!({"url": url})).await.unwrap();
            assert!(result.is_error, "{url} must be denied");
            let message = result.output["error"].as_str().unwrap();
            assert!(message.contains("SSRF"), "{url}: {message}");
        }
    }

    #[tokio::test]
    async fn web_fetch_rejects_invalid_arguments() {
        let tool = tool("web_fetch", WebToolsConfig::default());
        assert!(tool.execute(&context(), json!({})).await.is_err());
        assert!(tool
            .execute(&context(), json!({"url": "  "}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn web_fetch_strips_html() {
        // Serve a small HTML page on loopback and fetch it through the searxng
        // policy path is not wired for web_fetch — instead exercise the strip
        // pipeline directly.
        let (script, tag, whitespace) = html_strip();
        let html = "<html><head><style>x{y}</style><script>evil()</script></head><body><p>Hello <b>world</b></p></body></html>";
        let text = script.replace_all(html, " ");
        let text = tag.replace_all(&text, " ");
        let text = whitespace.replace_all(&text, " ").trim().to_owned();
        assert_eq!(text, "Hello world");
    }

    #[tokio::test]
    async fn searxng_loopback_search_round_trips() {
        // A self-hosted SearXNG on loopback: the only path allowed loopback egress.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let count = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("GET /search?"));
            assert!(request.contains("q=rust"));
            let body = json!({"results": [{"title": "Rust", "url": "https://rust-lang.org", "content": "the language"}]})
                .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let tool = tool(
            "web_search",
            WebToolsConfig {
                search_provider: "searxng".to_owned(),
                searxng_url: format!("http://{address}"),
                ..WebToolsConfig::default()
            },
        );
        let result = tool
            .execute(&context(), json!({"query": "rust"}))
            .await
            .unwrap();
        server.await.unwrap();
        assert!(!result.is_error, "{:?}", result.output);
        let text = result.output["text"].as_str().unwrap();
        assert!(text.contains("rust-lang.org"), "{text}");
    }

    #[test]
    fn allowed_domains_parse_json_and_csv() {
        assert_eq!(
            parse_allowed_domains(r#"["a.dev", " b.dev "]"#),
            vec!["a.dev", "b.dev"]
        );
        assert_eq!(parse_allowed_domains("a.dev, b.dev"), vec!["a.dev", "b.dev"]);
        assert!(parse_allowed_domains("").is_empty());
    }
}
