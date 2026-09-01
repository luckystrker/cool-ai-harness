use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{Tool, ToolContext, ToolDefinition, ToolError, ToolHandler, ToolResult};
use cool_security::sanitize_environment;
use cool_security::{Capability, Decision};
use futures_util::StreamExt as _;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::McpServer;

const MAX_MESSAGE_BYTES: usize = 1_048_576;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolPolicy {
    pub enabled: Option<BTreeSet<String>>,
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

impl McpToolPolicy {
    pub fn deny_all() -> Self {
        Self {
            enabled: Some(BTreeSet::new()),
            disabled: BTreeSet::new(),
        }
    }

    fn allows(&self, name: &str) -> bool {
        self.enabled
            .as_ref()
            .is_none_or(|names| names.contains(name))
            && !self.disabled.contains(name)
    }
}

#[derive(Debug)]
pub enum McpError {
    Io(std::io::Error),
    Http(reqwest::Error),
    Json(serde_json::Error),
    Protocol(String),
    Network(String),
    Timeout,
    SessionExpired,
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "MCP I/O error: {error}"),
            Self::Http(error) => write!(formatter, "MCP HTTP error: {error}"),
            Self::Json(error) => write!(formatter, "MCP JSON error: {error}"),
            Self::Protocol(message) => write!(formatter, "MCP protocol error: {message}"),
            Self::Network(message) => write!(formatter, "MCP network denied: {message}"),
            Self::Timeout => formatter.write_str("MCP request timed out"),
            Self::SessionExpired => formatter.write_str("MCP session expired"),
        }
    }
}

impl std::error::Error for McpError {}
impl From<std::io::Error> for McpError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for McpError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
impl From<reqwest::Error> for McpError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

#[derive(Clone)]
pub struct McpClient {
    server: McpServer,
    timeout: Duration,
}

impl McpClient {
    pub fn new(server: McpServer) -> Self {
        Self {
            server,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, value: Duration) -> Self {
        self.timeout = value;
        self
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Protocol("tools/list result has no tools array".to_owned()))?;
        tools
            .iter()
            .map(|tool| {
                let object = tool
                    .as_object()
                    .ok_or_else(|| McpError::Protocol("tool must be an object".to_owned()))?;
                Ok(McpTool {
                    name: object
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| McpError::Protocol("tool name is missing".to_owned()))?
                        .to_owned(),
                    description: object
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    input_schema: object
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object"})),
                    annotations: object.get("annotations").cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        if name.is_empty() || !arguments.is_object() {
            return Err(McpError::Protocol(
                "tool call requires a name and object arguments".to_owned(),
            ));
        }
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        match &self.server {
            McpServer::Stdio {
                command,
                args,
                env,
                cwd,
                ..
            } => {
                let mut environment = crate::execution_environment_baseline();
                environment.extend(sanitize_environment(
                    env.iter()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                    &Default::default(),
                ));
                let mut child = Command::new(command)
                    .args(args)
                    .current_dir(cwd)
                    .env_clear()
                    .envs(environment)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()?;
                let result = async {
                    let mut stdin = child.stdin.take().ok_or_else(|| {
                        McpError::Protocol("stdio stdin missing".to_owned())
                    })?;
                    let stdout = child.stdout.take().ok_or_else(|| {
                        McpError::Protocol("stdio stdout missing".to_owned())
                    })?;
                    let mut reader = BufReader::new(stdout);
                    write_line(&mut stdin, &json_rpc(1, "initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cool","version":env!("CARGO_PKG_VERSION")}}))).await?;
                    let initialized = read_response(&mut reader, 1, self.timeout).await?;
                    validate_initialize(&initialized)?;
                    write_line(
                        &mut stdin,
                        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                    )
                    .await?;
                    write_line(&mut stdin, &json_rpc(2, method, params)).await?;
                    read_response(&mut reader, 2, self.timeout).await
                }
                .await;
                let _ = child.kill().await;
                let _ = child.wait().await;
                result
            }
            McpServer::StreamableHttp { url, headers, .. } => {
                let url = url
                    .parse::<url::Url>()
                    .map_err(|error| McpError::Network(error.to_string()))?;
                let host = url
                    .host_str()
                    .ok_or_else(|| McpError::Network("missing host".to_owned()))?;
                let port = url
                    .port_or_known_default()
                    .ok_or_else(|| McpError::Network("missing port".to_owned()))?;
                let resolved = timeout(self.timeout, tokio::net::lookup_host((host, port)))
                    .await
                    .map_err(|_| McpError::Timeout)??
                    .collect::<Vec<SocketAddr>>();
                let policy = if host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
                {
                    cool_security::NetworkPolicy::new([host.to_owned()]).loopback_only()
                } else {
                    cool_security::NetworkPolicy::new([host.to_owned()])
                };
                let pinned = policy
                    .pin(url.as_str(), resolved.iter().map(SocketAddr::ip))
                    .map_err(|error| McpError::Network(error.to_string()))?;
                let address = resolved
                    .iter()
                    .find(|candidate| pinned.addresses.contains(&candidate.ip()))
                    .copied()
                    .ok_or_else(|| McpError::Network("no pinned socket".to_owned()))?;
                let client = reqwest::Client::builder()
                    .redirect(Policy::none())
                    .timeout(self.timeout)
                    .no_proxy()
                    .resolve(host, address)
                    .build()?;
                // OAuth token acquisition is intentionally outside this client. An embedding may
                // inject a reviewed Authorization header; redirects and credential URLs stay denied.
                http_session_request(&client, &url, headers, method, params).await
            }
        }
    }
}

async fn http_session_request(
    client: &reqwest::Client,
    url: &url::Url,
    headers: &std::collections::BTreeMap<String, String>,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    for attempt in 0..2 {
        let (initialized, session) = http_exchange(
            client,
            url,
            headers,
            None,
            json_rpc(1, "initialize", json!({"protocolVersion":MCP_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"cool","version":env!("CARGO_PKG_VERSION")}})),
            Some(1),
        )
        .await?;
        validate_initialize(&initialized)?;
        let flow = async {
            http_notification(
                client,
                url,
                headers,
                session.as_deref(),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            )
            .await?;
            let (result, _) = http_exchange(
                client,
                url,
                headers,
                session.as_deref(),
                json_rpc(2, method, params.clone()),
                Some(2),
            )
            .await?;
            Ok(result)
        }
        .await;
        match flow {
            Err(McpError::SessionExpired) if attempt == 0 => continue,
            result => return result,
        }
    }
    Err(McpError::SessionExpired)
}

async fn http_notification(
    client: &reqwest::Client,
    url: &url::Url,
    headers: &std::collections::BTreeMap<String, String>,
    session: Option<&str>,
    payload: Value,
) -> Result<(), McpError> {
    let mut request = client.post(url.clone());
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request = request
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json");
    if let Some(session) = session {
        request = request
            .header("mcp-session-id", session)
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION);
    }
    let response = request.json(&payload).send().await?;
    if response.status().is_redirection() {
        return Err(McpError::Network(
            "redirect requires explicit revalidation".to_owned(),
        ));
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND && session.is_some() {
        return Err(McpError::SessionExpired);
    }
    if !response.status().is_success() {
        return Err(McpError::Protocol(format!(
            "HTTP status {}",
            response.status()
        )));
    }
    Ok(())
}

async fn http_exchange(
    client: &reqwest::Client,
    url: &url::Url,
    headers: &std::collections::BTreeMap<String, String>,
    session: Option<&str>,
    payload: Value,
    expected_id: Option<u64>,
) -> Result<(Value, Option<String>), McpError> {
    let mut request = client.post(url.clone());
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request = request
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json");
    if let Some(session) = session {
        request = request
            .header("mcp-session-id", session)
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION);
    }
    let response = request.json(&payload).send().await?;
    if response.status().is_redirection() {
        return Err(McpError::Network(
            "redirect requires explicit revalidation".to_owned(),
        ));
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND && session.is_some() {
        return Err(McpError::SessionExpired);
    }
    if !response.status().is_success() {
        return Err(McpError::Protocol(format!(
            "HTTP status {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_MESSAGE_BYTES as u64)
    {
        return Err(McpError::Protocol("response is too large".to_owned()));
    }
    let session = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| session.map(str::to_owned));
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let value = bounded_http_json(response, &content_type, expected_id).await?;
    let value = if content_type.starts_with("text/event-stream") {
        value
    } else if let Some(id) = expected_id {
        parse_response(value, id)?
    } else {
        value
    };
    Ok((value, session))
}

pub async fn discover_plugin_tools(plugin: &str, server: McpServer) -> Result<Vec<Tool>, McpError> {
    discover_plugin_tools_with_policy(plugin, server, &McpToolPolicy::default()).await
}

pub async fn discover_plugin_tools_with_policy(
    plugin: &str,
    server: McpServer,
    policy: &McpToolPolicy,
) -> Result<Vec<Tool>, McpError> {
    // MCP annotations are untrusted hints, not authority. Until a core-owned per-tool override is
    // configured, require the full side-effect set so transport alone cannot launder write/git/send.
    let capabilities = vec![
        Capability::Read,
        Capability::Write,
        Capability::Execute,
        Capability::Network,
        Capability::Git,
        Capability::SendExternal,
    ];
    let server_name = server.name().to_owned();
    let client = McpClient::new(server);
    let definitions = client.list_tools().await?;
    Ok(definitions
        .into_iter()
        .filter(|definition| policy.allows(&runtime_name(plugin, &server_name, &definition.name)))
        .map(|definition| {
            let name = runtime_name(plugin, &server_name, &definition.name);
            Tool::new(
                ToolDefinition {
                    name,
                    description: definition
                        .description
                        .unwrap_or_else(|| "Plugin MCP tool".to_owned()),
                    parameters: definition.input_schema,
                },
                capabilities.clone(),
                Decision::Ask,
                McpToolHandler {
                    client: client.clone(),
                    remote_name: definition.name,
                },
            )
        })
        .collect())
}

#[derive(Clone)]
struct McpToolHandler {
    client: McpClient,
    remote_name: String,
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        Ok(
            match self.client.call_tool(&self.remote_name, arguments).await {
                Ok(value) => ToolResult::ok(value).masked(),
                Err(error) => ToolResult::error("plugin_mcp_failed", error.to_string()),
            },
        )
    }
}

fn runtime_name(plugin: &str, server: &str, tool: &str) -> String {
    let normalize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!(
        "plugin_{}_{}_{}",
        normalize(plugin),
        normalize(server),
        normalize(tool)
    )
}

fn json_rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

async fn write_line(stdin: &mut tokio::process::ChildStdin, value: &Value) -> Result<(), McpError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_response(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    duration: Duration,
) -> Result<Value, McpError> {
    timeout(duration, async {
        loop {
            let line = read_bounded_line(reader).await?;
            if line.is_empty() {
                return Err(McpError::Protocol("server closed stdout".to_owned()));
            }
            let value: Value = serde_json::from_slice(&line)?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return parse_response(value, id);
            }
        }
    })
    .await
    .map_err(|_| McpError::Timeout)?
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, McpError> {
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(output);
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len() + take > MAX_MESSAGE_BYTES {
            return Err(McpError::Protocol("response is too large".to_owned()));
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if output.last() == Some(&b'\n') {
            return Ok(output);
        }
    }
}

async fn bounded_http_json(
    response: reqwest::Response,
    content_type: &str,
    expected_id: Option<u64>,
) -> Result<Value, McpError> {
    let mut bytes = Vec::new();
    let mut received = 0_usize;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received = received.saturating_add(chunk.len());
        if received > MAX_MESSAGE_BYTES {
            return Err(McpError::Protocol("response is too large".to_owned()));
        }
        bytes.extend_from_slice(&chunk);
        if content_type.starts_with("text/event-stream") {
            while let Some((end, delimiter_len)) = next_sse_block(&bytes) {
                let block = bytes.drain(..end).collect::<Vec<_>>();
                bytes.drain(..delimiter_len);
                if let Some(value) = parse_sse_block(&block, expected_id)? {
                    return Ok(value);
                }
            }
        }
    }
    if content_type.starts_with("text/event-stream") {
        if !bytes.is_empty()
            && let Some(value) = parse_sse_block(&bytes, expected_id)?
        {
            return Ok(value);
        }
        return Err(McpError::Protocol(
            "SSE response has no matching JSON-RPC event".to_owned(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn next_sse_block(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|end| (end, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|end| (end, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn parse_sse_block(block: &[u8], expected_id: Option<u64>) -> Result<Option<Value>, McpError> {
    let text = std::str::from_utf8(block)
        .map_err(|error| McpError::Protocol(error.to_string()))?
        .replace("\r\n", "\n");
    let data = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&data)?;
    if expected_id.is_some_and(|id| value.get("id").and_then(Value::as_u64) != Some(id)) {
        return Ok(None);
    }
    expected_id.map_or(Ok(Some(value.clone())), |id| {
        parse_response(value, id).map(Some)
    })
}

fn validate_initialize(result: &Value) -> Result<(), McpError> {
    if result.get("protocolVersion").and_then(Value::as_str) != Some(MCP_PROTOCOL_VERSION)
        || !result
            .get("capabilities")
            .is_none_or(serde_json::Value::is_object)
    {
        return Err(McpError::Protocol(
            "MCP initialize negotiated an unsupported protocol or capabilities shape".to_owned(),
        ));
    }
    Ok(())
}

fn parse_response(value: Value, id: u64) -> Result<Value, McpError> {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_u64) != Some(id)
    {
        return Err(McpError::Protocol("invalid JSON-RPC envelope".to_owned()));
    }
    if let Some(error) = value.get("error") {
        return Err(McpError::Protocol(error.to_string()));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Protocol("JSON-RPC result is missing".to_owned()))
}
