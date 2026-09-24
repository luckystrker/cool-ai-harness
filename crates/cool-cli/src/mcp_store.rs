//! Public MCP Registry client (M11 WS1d / Workstream B4b).
//!
//! Ports `backend/app/mcp/marketplace.py`: one unauthenticated
//! `GET https://registry.modelcontextprotocol.io/v0/servers` endpoint behind
//! the Rust egress policy. Deliberate divergences from Python, recorded:
//! - Egress is `NetworkPolicy`-pinned to the registry host (Python uses bare
//!   `httpx` with no SSRF check); redirects are refused (`Policy::none()`),
//!   Python follows them.
//! - 15 s timeout (Python parity) and `User-Agent: CoolAIHarness/0.1`
//!   (Python sends the httpx default).
//! - The limit clamp `min(limit, 50)` is applied here (Python clamps in the
//!   API layer only).
//!
//! The registry response is untrusted: every string is bounded before it is
//! returned, and `registry_entry_to_config` reproduces Python's name
//! derivation and package scoring exactly (including its quirks).

use std::time::Duration;

use cool_protocol::{McpAddServerParams, McpStoreItemRecord, McpStoreSearchResult};
use cool_security::NetworkPolicy;
use cool_security::mask_secrets;
use serde_json::Value;

const REGISTRY_BASE_URL: &str = "https://registry.modelcontextprotocol.io";
/// Python `_TIMEOUT_S = 15.0`.
const TIMEOUT_S: u64 = 15;
const USER_AGENT: &str = "CoolAIHarness/0.1";
/// Python API-layer clamp (`min(limit, 50)`).
const MAX_LIMIT: u16 = 50;
const MAX_FIELD_CHARS: usize = 4_000;
/// Registry response body cap (untrusted JSON must not exhaust memory).
const MAX_RESPONSE_BYTES: usize = 5_000_000;
/// Bounded to the reused `add_server` validators (Python's install path skips
/// them; a 501-char description would otherwise fail the 500-char check).
const MAX_DESCRIPTION_CHARS: usize = 500;
const MAX_NAME_CHARS: usize = 64;

/// Registry transport kind, matching Python `registry_entry_to_config`.
#[derive(Clone, Debug, PartialEq)]
pub enum RegistryConfig {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        description: String,
    },
    Http {
        name: String,
        url: String,
        description: String,
    },
}

impl RegistryConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio { name, .. } | Self::Http { name, .. } => name,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Stdio { description, .. } | Self::Http { description, .. } => description,
        }
    }
}

/// `GET /v0/servers` with pinned egress. `search` is omitted when empty.
async fn fetch_servers(search: Option<&str>, limit: u16) -> Result<Value, String> {
    let mut url = url::Url::parse(&format!("{REGISTRY_BASE_URL}/v0/servers"))
        .map_err(|error| format!("invalid registry URL: {error}"))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("limit", &limit.min(MAX_LIMIT).to_string());
        if let Some(search) = search.filter(|value| !value.is_empty()) {
            query.append_pair("search", search);
        }
    }
    // The registry host is a fixed constant, never caller-controlled.
    let host = match url.host() {
        Some(url::Host::Domain(domain)) => domain.to_ascii_lowercase(),
        Some(url::Host::Ipv4(address)) => address.to_string(),
        Some(url::Host::Ipv6(address)) => address.to_string(),
        None => return Err("registry URL has no host".to_owned()),
    };
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "registry URL has no port".to_owned())?;
    let resolved = tokio::time::timeout(
        Duration::from_secs(TIMEOUT_S),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| "registry host lookup timed out".to_owned())?
    .map_err(|error| format!("registry host lookup failed: {error}"))?
    .collect::<Vec<_>>();
    let policy = NetworkPolicy::new([host.clone()]);
    let pinned = policy
        .pin(url.as_str(), resolved.iter().map(|socket| socket.ip()))
        .map_err(|error| format!("registry URL denied: {error}"))?;
    let address = resolved
        .iter()
        .find(|candidate| pinned.addresses.contains(&candidate.ip()))
        .copied()
        .ok_or_else(|| "no pinned registry address".to_owned())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(TIMEOUT_S))
        .no_proxy()
        .resolve(&host, address)
        .build()
        .map_err(|error| format!("registry client failed: {error}"))?;
    let response = client
        .get(url)
        .header("user-agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| format!("registry request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "registry returned HTTP {}",
            response.status().as_u16()
        ));
    }
    // Cap the untrusted body before parsing (bounded memory, Python has no cap).
    let mut response = response;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("registry body failed: {error}"))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!("registry body exceeds {MAX_RESPONSE_BYTES} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<Value>(&body).map_err(|_| "registry response was not JSON".to_owned())
}

/// Registry error whose variants the install path can discriminate without
/// string matching.
#[derive(Debug)]
pub enum RegistryLookupError {
    NotFound,
    Failed(String),
}

fn bounded(value: &str) -> String {
    value.chars().take(MAX_FIELD_CHARS).collect()
}

fn bounded_description(value: &str) -> String {
    // `validate_description` counts UTF-8 bytes (`str::len`), so truncate on a
    // char boundary to ≤500 bytes — 500 chars would fail on multi-byte text.
    truncate_to_bytes(value, MAX_DESCRIPTION_CHARS)
}

fn bounded_name(value: &str) -> String {
    value.chars().take(MAX_NAME_CHARS).collect()
}

/// Truncate to at most `max_bytes` UTF-8 bytes without splitting a char.
fn truncate_to_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Python truthiness for a JSON default (`None`/`""`/`0`/`false`/`[]`/`{}` are falsy).
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::String(text) => !text.is_empty(),
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python `str(default_val)` for the scalar shapes a package argument carries.
fn json_default_to_arg(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// Python `_pick_best_package`: stdio +10 / streamable-http +5 / sse +3,
/// runtimeHint npx +2 / uvx +1. Stable order keeps registry order on ties.
fn pick_best_package(packages: &[Value]) -> Option<&Value> {
    let mut scored: Vec<(i64, usize, &Value)> = Vec::new();
    for (index, package) in packages.iter().enumerate() {
        let transport_type = package
            .get("transport")
            .and_then(Value::as_object)
            .and_then(|transport| transport.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let runtime = package
            .get("runtimeHint")
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut score = 0i64;
        match transport_type {
            "stdio" => score += 10,
            "streamable-http" => score += 5,
            "sse" => score += 3,
            _ => {}
        }
        match runtime {
            "npx" => score += 2,
            "uvx" => score += 1,
            _ => {}
        }
        scored.push((score, index, package));
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored.first().map(|(_, _, package)| *package)
}

/// Python `_summarize_entry`.
fn summarize_entry(server: &Value) -> Option<McpStoreItemRecord> {
    let name = bounded(server.get("name").and_then(Value::as_str).unwrap_or(""));
    if name.is_empty() {
        return None;
    }
    let description = bounded(
        server
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let version = bounded(server.get("version").and_then(Value::as_str).unwrap_or(""));
    let repository_url = bounded(
        server
            .get("repository")
            .and_then(Value::as_object)
            .and_then(|repository| repository.get("url"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let packages = server.get("packages").and_then(Value::as_array);
    let packages_count = packages.map_or(0, |packages: &Vec<Value>| packages.len()) as i64;
    let best = packages.and_then(|packages| pick_best_package(packages));
    let mut install_command = String::new();
    let mut transport = String::new();
    if let Some(best) = best {
        let registry_type = best
            .get("registryType")
            .and_then(Value::as_str)
            .unwrap_or("npm");
        let identifier = best.get("identifier").and_then(Value::as_str).unwrap_or("");
        let runtime_hint = best
            .get("runtimeHint")
            .and_then(Value::as_str)
            .unwrap_or("");
        install_command = bounded(&match registry_type {
            "npm" => format!(
                "{} -y {identifier}",
                if runtime_hint.is_empty() {
                    "npx"
                } else {
                    runtime_hint
                }
            ),
            "pypi" => format!(
                "{} {identifier}",
                if runtime_hint.is_empty() {
                    "uvx"
                } else {
                    runtime_hint
                }
            ),
            _ => identifier.to_owned(),
        });
        transport = bounded(
            best.get("transport")
                .and_then(Value::as_object)
                .and_then(|info| info.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("stdio"),
        );
    }
    Some(McpStoreItemRecord {
        name,
        description,
        version,
        repository_url,
        install_command,
        transport,
        packages_count,
    })
}

/// Python `registry_entry_to_config`: name derivation, package scoring and
/// npm/pypi command construction. `None` when there is no installable package.
pub fn registry_entry_to_config(entry: &Value, local_name: &str) -> Result<RegistryConfig, String> {
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    // Bounded to `validate_description` (500) so the reused `add_server`
    // accepts real registry entries (Python's install path skips validation).
    let description = bounded_description(
        entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let packages = entry.get("packages").and_then(Value::as_array);
    let Some(packages) = packages.filter(|packages| !packages.is_empty()) else {
        return Err("no installable packages".to_owned());
    };
    let best = pick_best_package(packages).ok_or_else(|| "no installable packages".to_owned())?;

    let mut derived = if local_name.is_empty() {
        let tail = name.rsplit('/').next().unwrap_or(&name).to_owned();
        let stripped = ["servers-", "mcp-server-", "mcp-"]
            .iter()
            .find_map(|prefix| tail.strip_prefix(*prefix))
            .unwrap_or(&tail)
            .to_owned();
        stripped
            .to_lowercase()
            .replace([' ', '_'], "-")
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect::<String>()
    } else {
        // Python uses `local_name` raw (no trim); kept raw for parity, then
        // bounded for `validate_name`.
        local_name.to_owned()
    };
    derived = bounded_name(&derived);
    if derived.is_empty() {
        derived = "server".to_owned();
    }

    let transport_info = best.get("transport").and_then(Value::as_object);
    let transport_type = transport_info
        .and_then(|info| info.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("stdio");
    let identifier = best.get("identifier").and_then(Value::as_str).unwrap_or("");
    let runtime_hint = best
        .get("runtimeHint")
        .and_then(Value::as_str)
        .unwrap_or("");
    let registry_type = best
        .get("registryType")
        .and_then(Value::as_str)
        .unwrap_or("npm");

    let mut command = match registry_type {
        "npm" => {
            if runtime_hint.is_empty() {
                "npx".to_owned()
            } else {
                runtime_hint.to_owned()
            }
        }
        "pypi" => {
            if runtime_hint.is_empty() {
                "uvx".to_owned()
            } else {
                runtime_hint.to_owned()
            }
        }
        _ => {
            if runtime_hint.is_empty() {
                identifier.to_owned()
            } else {
                runtime_hint.to_owned()
            }
        }
    };
    let mut args: Vec<String> = match registry_type {
        "npm" => vec!["-y".to_owned(), identifier.to_owned()],
        "pypi" => vec![identifier.to_owned()],
        _ => Vec::new(),
    };
    if let Some(package_arguments) = best.get("packageArguments").and_then(Value::as_array) {
        for package_argument in package_arguments {
            let required = package_argument
                .get("isRequired")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let default = package_argument
                .get("default")
                .filter(|value| is_truthy(value));
            // Python: skip `isRequired and not default` (left for the user).
            if required && default.is_none() {
                continue;
            }
            let argument_name = package_argument
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("");
            // Python: append only when both `name` and `default` are truthy.
            if !argument_name.is_empty()
                && let Some(default) = default
            {
                args.push(format!("--{argument_name}"));
                args.push(json_default_to_arg(default));
            }
        }
    }
    command = bounded(&command);
    args = args.iter().map(|argument| bounded(argument)).collect();

    if matches!(transport_type, "sse" | "streamable-http" | "http") {
        let url_template = transport_info
            .and_then(|info| info.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("http://127.0.0.1:8080/mcp");
        let url = bounded(&url_template.replace("{port}", "8080"));
        return Ok(RegistryConfig::Http {
            name: derived,
            url,
            description,
        });
    }
    Ok(RegistryConfig::Stdio {
        name: derived,
        command,
        args,
        description,
    })
}

/// Registry search (`search` is sent only when non-empty; the echoed `query`
/// stays raw, matching Python).
pub async fn search_registry(query: &str, limit: u16) -> Result<McpStoreSearchResult, String> {
    let search = query.trim();
    let payload = fetch_servers(
        if search.is_empty() {
            None
        } else {
            Some(search)
        },
        limit,
    )
    .await?;
    let results = summarize_list(&payload);
    Ok(McpStoreSearchResult {
        results,
        query: query.to_owned(),
    })
}

/// Registry popular list (Python echoes `query: ""`).
pub async fn list_popular(limit: u16) -> Result<McpStoreSearchResult, String> {
    let payload = fetch_servers(None, limit).await?;
    Ok(McpStoreSearchResult {
        results: summarize_list(&payload),
        query: String::new(),
    })
}

/// Exact-name registry lookup (Python `get_server_details`, `limit=20`).
pub async fn get_server_details(registry_name: &str) -> Result<Value, RegistryLookupError> {
    // Python always sends `search=` (even empty); `fetch_servers` omits empty
    // search, which only matters for an empty `registry_name` (no such entry).
    let payload = fetch_servers(Some(registry_name), 20)
        .await
        .map_err(RegistryLookupError::Failed)?;
    let servers = payload
        .get("servers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for entry in servers {
        let server = entry.get("server").cloned().unwrap_or(Value::Null);
        if server.get("name").and_then(Value::as_str) == Some(registry_name) {
            return Ok(server);
        }
    }
    Err(RegistryLookupError::NotFound)
}

fn summarize_list(payload: &Value) -> Vec<McpStoreItemRecord> {
    payload
        .get("servers")
        .and_then(Value::as_array)
        .map(|servers| {
            servers
                .iter()
                .filter_map(|entry| entry.get("server"))
                .filter_map(summarize_entry)
                .collect()
        })
        .unwrap_or_default()
}

/// Build the `add_server` params Python would derive from a registry entry.
pub fn registry_config_to_add_params(
    config: &RegistryConfig,
    idempotency_key: &str,
) -> McpAddServerParams {
    let (transport, command, args, url) = match config {
        RegistryConfig::Stdio { command, args, .. } => {
            ("stdio", command.clone(), args.clone(), String::new())
        }
        RegistryConfig::Http { url, .. } => ("http", String::new(), Vec::new(), url.clone()),
    };
    McpAddServerParams {
        // Pre-validated at deserialization; the literal is unreachable.
        idempotency_key: cool_protocol::IdempotencyKey::new(idempotency_key)
            .unwrap_or_else(|_| cool_protocol::IdempotencyKey::new("install").expect("literal")),
        name: config.name().to_owned(),
        transport: transport.to_owned(),
        command,
        args,
        // Registry JSON must never inject credentials.
        env: Default::default(),
        url,
        headers: Default::default(),
        enabled: true,
        description: bounded_description(config.description()),
        capabilities: Vec::new(),
        timeout_s: 30.0,
        version: String::new(),
        author: String::new(),
        compatibility: String::new(),
    }
}

/// Mask an error before it is returned as a store/host detail.
pub fn masked_registry_error(error: &str) -> String {
    mask_secrets(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summarize_maps_fields_and_install_command() {
        let server = json!({
            "name": "io.github.example/servers-filesystem",
            "description": "Read files",
            "version": "1.2.3",
            "repository": {"url": "https://github.com/example/registry"},
            "packages": [
                {"registryType": "npm", "identifier": "@example/fs", "runtimeHint": "npx",
                 "transport": {"type": "stdio"}}
            ]
        });
        let item = summarize_entry(&server).expect("summary");
        assert_eq!(item.name, "io.github.example/servers-filesystem");
        assert_eq!(item.repository_url, "https://github.com/example/registry");
        assert_eq!(item.install_command, "npx -y @example/fs");
        assert_eq!(item.transport, "stdio");
        assert_eq!(item.packages_count, 1);
    }

    #[test]
    fn summarize_skips_entries_without_a_name() {
        assert!(summarize_entry(&json!({"description": "x"})).is_none());
    }

    #[test]
    fn best_package_scoring_prefers_stdio_then_npx() {
        let packages = json!([
            {"registryType": "npm", "identifier": "a", "transport": {"type": "sse"}},
            {"registryType": "npm", "identifier": "b", "runtimeHint": "npx",
             "transport": {"type": "stdio"}},
            {"registryType": "npm", "identifier": "c", "transport": {"type": "streamable-http"}}
        ]);
        let best = pick_best_package(packages.as_array().expect("array")).expect("best");
        assert_eq!(best["identifier"], "b");
    }

    #[test]
    fn best_package_keeps_registry_order_on_ties() {
        let packages = json!([
            {"registryType": "npm", "identifier": "first", "transport": {"type": "stdio"}},
            {"registryType": "npm", "identifier": "second", "transport": {"type": "stdio"}}
        ]);
        let best = pick_best_package(packages.as_array().expect("array")).expect("best");
        assert_eq!(best["identifier"], "first");
    }

    #[test]
    fn config_derives_local_name_and_builds_npm_args() {
        let entry = json!({
            "name": "io.github.modelcontextprotocol/servers-filesystem",
            "description": "Read files",
            "packages": [{
                "registryType": "npm",
                "identifier": "@modelcontextprotocol/server-filesystem",
                "transport": {"type": "stdio"},
                "packageArguments": [
                    {"name": "allow", "isRequired": true, "default": ""},
                    {"name": "depth", "isRequired": false, "default": "3"}
                ]
            }]
        });
        let config = registry_entry_to_config(&entry, "").expect("config");
        match &config {
            RegistryConfig::Stdio {
                name,
                command,
                args,
                description,
            } => {
                assert_eq!(name, "filesystem");
                assert_eq!(command, "npx");
                assert_eq!(
                    *args,
                    vec![
                        "-y".to_owned(),
                        "@modelcontextprotocol/server-filesystem".to_owned(),
                        "--depth".to_owned(),
                        "3".to_owned()
                    ],
                    "required-without-default is skipped; defaults become --name value"
                );
                assert_eq!(description, "Read files");
            }
            other => panic!("expected stdio, got {other:?}"),
        }
    }

    #[test]
    fn config_http_transport_takes_the_url_and_drops_command() {
        let entry = json!({
            "name": "remote",
            "description": "Remote",
            "packages": [{
                "registryType": "npm",
                "identifier": "unused",
                "transport": {"type": "streamable-http", "url": "http://127.0.0.1:{port}/mcp"}
            }]
        });
        let config = registry_entry_to_config(&entry, "custom").expect("config");
        match &config {
            RegistryConfig::Http { name, url, .. } => {
                assert_eq!(name, "custom", "explicit local_name wins");
                assert_eq!(url, "http://127.0.0.1:8080/mcp");
            }
            other => panic!("expected http, got {other:?}"),
        }
    }

    #[test]
    fn config_rejects_entries_without_packages() {
        let entry = json!({"name": "empty", "packages": []});
        let error = registry_entry_to_config(&entry, "").expect_err("no packages");
        assert!(error.contains("no installable packages"), "{error}");
    }

    #[test]
    fn summarize_list_skips_nameless_entries() {
        let payload = json!({"servers": [{"server": {"name": "a", "packages": []}}, {"server": {"description": "x"}}]});
        let results = summarize_list(&payload);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "a");
    }

    #[test]
    fn package_argument_defaults_handle_scalars_and_required_gaps() {
        let entry = json!({
            "name": "pkg",
            "packages": [{
                "registryType": "pypi",
                "identifier": "tool",
                "transport": {"type": "stdio"},
                "packageArguments": [
                    {"name": "depth", "isRequired": false, "default": 3},
                    {"name": "flag", "isRequired": false, "default": true},
                    {"name": "missing", "isRequired": true},
                    {"name": "empty", "isRequired": false, "default": ""}
                ]
            }]
        });
        let config = registry_entry_to_config(&entry, "").expect("config");
        match config {
            RegistryConfig::Stdio { args, .. } => {
                assert_eq!(
                    args,
                    vec![
                        "tool".to_owned(),
                        "--depth".to_owned(),
                        "3".to_owned(),
                        "--flag".to_owned(),
                        "True".to_owned()
                    ],
                    "numeric/bool defaults stringify like Python str(); required-without-default and empty defaults are skipped"
                );
            }
            other => panic!("expected stdio, got {other:?}"),
        }
    }

    #[test]
    fn config_bounds_name_and_description_for_add_server_validators() {
        let entry = json!({
            "name": "n".repeat(80),
            "description": "d".repeat(600),
            "packages": [{
                "registryType": "npm", "identifier": "x",
                "transport": {"type": "stdio"}
            }]
        });
        let config = registry_entry_to_config(&entry, "").expect("config");
        assert_eq!(config.name().chars().count(), 64);
        assert_eq!(config.description().len(), 500, "bounded in bytes");

        // Multi-byte descriptions must land inside the 500-byte validator.
        let entry = json!({
            "name": "mb",
            "description": "é".repeat(400),
            "packages": [{
                "registryType": "npm", "identifier": "x",
                "transport": {"type": "stdio"}
            }]
        });
        let config = registry_entry_to_config(&entry, "").expect("config");
        assert!(
            config.description().len() <= 500,
            "multi-byte description must fit validate_description's 500-byte bound, got {}",
            config.description().len()
        );
        assert!(
            config.description().ends_with('é') || config.description().chars().all(|c| c == 'é')
        );
    }

    #[test]
    fn falsy_numeric_defaults_are_skipped() {
        let entry = json!({
            "name": "pkg",
            "packages": [{
                "registryType": "pypi",
                "identifier": "tool",
                "transport": {"type": "stdio"},
                "packageArguments": [
                    {"name": "zero", "isRequired": false, "default": 0},
                    {"name": "off", "isRequired": false, "default": false},
                    {"name": "kept", "isRequired": false, "default": 2}
                ]
            }]
        });
        let config = registry_entry_to_config(&entry, "").expect("config");
        match config {
            RegistryConfig::Stdio { args, .. } => {
                assert_eq!(
                    args,
                    vec!["tool".to_owned(), "--kept".to_owned(), "2".to_owned()],
                    "Python `not 0`/`not False` skip those defaults"
                );
            }
            other => panic!("expected stdio, got {other:?}"),
        }
    }
}
