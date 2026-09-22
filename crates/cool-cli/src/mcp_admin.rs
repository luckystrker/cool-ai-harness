//! Operator-owned global MCP server admin (M11 WS1d).
//!
//! Python kept MCP servers in `config.yaml` with a live asyncio registry. The
//! Rust core has no global MCP config: MCP servers are either plugin-bundled
//! (projected read-only by `extensions.status`) or, from here on, operator
//! configs stored in a small JSON document on the data root. The live session
//! registry is in-process: `McpClient` is stateless (it spawns/HTTP-calls per
//! request), so "connect" is a `tools/list` round-trip and the discovered tools
//! are cached until disconnect.
//!
//! Secret safety: `env`/`headers` values are accepted on writes and interpolated
//! (`${VAR}`) at connect time, but are never projected back or logged.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use cool_app_server::McpAdmin;
use cool_extensions::{McpClient, McpError, McpServer, McpTool};
use cool_protocol::{
    McpAddServerParams, McpConnectResult, McpHealthResult, McpServerAdminRecord,
    McpServerListResult, McpToolListResult, McpToolRecord, McpUpdateServerParams,
};
use cool_security::mask_secrets;
use serde::{Deserialize, Serialize};

const CONFIG_FILE: &str = "mcp-servers.json";
const AUDIT_FILE: &str = "mcp-admin-audit.jsonl";
/// A hand-edited config document is bounded so it cannot exhaust memory.
const MAX_CONFIG_BYTES: u64 = 1_048_576;

fn default_transport() -> String {
    "stdio".to_owned()
}

fn default_enabled() -> bool {
    true
}

fn default_timeout_s() -> f64 {
    30.0
}

/// One operator-configured MCP server. Mirrors the Python `MCPServerConfig`
/// fields the admin surface manages (plugin manifest metadata included).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default = "default_timeout_s")]
    pub timeout_s: f64,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub compatibility: String,
}

impl McpServerConfig {
    fn from_add(params: &McpAddServerParams) -> Self {
        Self {
            name: params.name.clone(),
            transport: params.transport.clone(),
            command: params.command.clone(),
            args: params.args.clone(),
            env: params.env.clone(),
            url: params.url.clone(),
            headers: params.headers.clone(),
            enabled: params.enabled,
            description: params.description.clone(),
            capabilities: params.capabilities.clone(),
            timeout_s: params.timeout_s,
            version: params.version.clone(),
            author: params.author.clone(),
            compatibility: params.compatibility.clone(),
        }
    }

    fn apply_update(&mut self, params: &McpUpdateServerParams) {
        if let Some(value) = &params.transport {
            self.transport = value.clone();
        }
        if let Some(value) = &params.command {
            self.command = value.clone();
        }
        if let Some(value) = &params.args {
            self.args = value.clone();
        }
        if let Some(value) = &params.env {
            self.env = value.clone();
        }
        if let Some(value) = &params.url {
            self.url = value.clone();
        }
        if let Some(value) = &params.headers {
            self.headers = value.clone();
        }
        if let Some(value) = params.enabled {
            self.enabled = value;
        }
        if let Some(value) = &params.description {
            self.description = value.clone();
        }
        if let Some(value) = &params.capabilities {
            self.capabilities = value.clone();
        }
        if let Some(value) = params.timeout_s {
            self.timeout_s = value;
        }
    }
}

/// The operator MCP config document on the data root, written atomically under a
/// process lock. A missing file is an empty config; a corrupt file fails closed.
pub struct McpConfigStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl McpConfigStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<Vec<McpServerConfig>, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "mcp config lock poisoned".to_owned())?;
        read_configs(&self.path)
    }

    fn write(&self, configs: &[McpServerConfig]) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "mcp config lock poisoned".to_owned())?;
        write_configs(&self.path, configs)
    }
}

fn read_configs(path: &Path) -> Result<Vec<McpServerConfig>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("mcp config unreadable: {error}")),
    };
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err("mcp config exceeds the size limit".to_owned());
    }
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("mcp config unreadable: {error}"))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let configs: Vec<McpServerConfig> =
        serde_json::from_str(&text).map_err(|_| "mcp config is not valid JSON".to_owned())?;
    Ok(configs)
}

fn write_configs(path: &Path, configs: &[McpServerConfig]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("mcp config dir failed: {error}"))?;
    }
    let body =
        serde_json::to_vec_pretty(configs).map_err(|_| "mcp config serialize failed".to_owned())?;
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&temp, &body).map_err(|error| format!("mcp config write failed: {error}"))?;
    std::fs::rename(&temp, path).map_err(|error| format!("mcp config replace failed: {error}"))
}

#[derive(Clone, Default)]
struct SessionState {
    status: String,
    tools: Vec<McpTool>,
    error: Option<String>,
}

impl SessionState {
    fn disconnected() -> Self {
        Self {
            status: "disconnected".to_owned(),
            tools: Vec::new(),
            error: None,
        }
    }
}

/// CLI MCP admin: the config store plus an in-process live session registry.
pub struct CliMcpAdmin {
    store: McpConfigStore,
    data_dir: PathBuf,
    sessions: Mutex<HashMap<String, SessionState>>,
    audit_path: PathBuf,
}

impl CliMcpAdmin {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        Self {
            store: McpConfigStore::open(data_dir.join(CONFIG_FILE)),
            audit_path: data_dir.join(AUDIT_FILE),
            data_dir,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn audit(&self, actor: &str, action: &str, target: &str, outcome: &str) {
        let record = serde_json::json!({
            "at": now_string(),
            "actor": actor,
            "action": action,
            "target": target,
            "outcome": outcome,
        });
        if let Ok(line) = serde_json::to_string(&record) {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.audit_path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn session(&self, name: &str) -> SessionState {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(name).cloned())
            .unwrap_or_else(SessionState::disconnected)
    }

    fn set_session(&self, name: &str, state: SessionState) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(name.to_owned(), state);
        }
    }

    fn drop_session(&self, name: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(name);
        }
    }

    /// Build a stateless MCP client for a config, interpolating `${VAR}` in the
    /// command/args/env/url/headers from the process environment.
    fn client(&self, config: &McpServerConfig) -> Result<McpClient, String> {
        let server = match config.transport.as_str() {
            "stdio" => McpServer::Stdio {
                name: config.name.clone(),
                command: PathBuf::from(interpolate(&config.command)),
                args: config.args.iter().map(|arg| interpolate(arg)).collect(),
                env: config
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), interpolate(value)))
                    .collect(),
                cwd: self.data_dir.clone(),
            },
            "http" => McpServer::StreamableHttp {
                name: config.name.clone(),
                url: interpolate(&config.url),
                headers: config
                    .headers
                    .iter()
                    .map(|(key, value)| (key.clone(), interpolate(value)))
                    .collect(),
            },
            other => return Err(format!("invalid transport {other:?}")),
        };
        let timeout = Duration::from_secs_f64(config.timeout_s.clamp(1.0, 300.0));
        Ok(McpClient::new(server).with_timeout(timeout))
    }

    fn record(&self, config: &McpServerConfig) -> McpServerAdminRecord {
        let session = self.session(&config.name);
        let status = if session.status.is_empty() {
            "disconnected".to_owned()
        } else {
            session.status
        };
        McpServerAdminRecord {
            name: config.name.clone(),
            transport: config.transport.clone(),
            status,
            enabled: config.enabled,
            description: config.description.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            url: config.url.clone(),
            capabilities: config.capabilities.clone(),
            timeout_s: config.timeout_s,
            version: config.version.clone(),
            author: config.author.clone(),
            compatibility: config.compatibility.clone(),
            error: session.error,
            tools: session
                .tools
                .iter()
                .map(|tool| tool_record(&config.name, tool))
                .collect(),
            // The stateless client does not surface the MCP `initialize` result.
            server_info: None,
        }
    }

    async fn connect_config(&self, config: &McpServerConfig) -> McpConnectResult {
        let client = match self.client(config) {
            Ok(client) => client,
            Err(message) => {
                let message = mask_secrets(&message);
                self.set_session(
                    &config.name,
                    SessionState {
                        status: "error".to_owned(),
                        tools: Vec::new(),
                        error: Some(message.clone()),
                    },
                );
                return McpConnectResult {
                    name: config.name.clone(),
                    status: "error".to_owned(),
                    tools_count: 0,
                    error: Some(message),
                };
            }
        };
        match client.list_tools().await {
            Ok(tools) => {
                let count = tools.len() as i64;
                self.set_session(
                    &config.name,
                    SessionState {
                        status: "connected".to_owned(),
                        tools,
                        error: None,
                    },
                );
                McpConnectResult {
                    name: config.name.clone(),
                    status: "connected".to_owned(),
                    tools_count: count,
                    error: None,
                }
            }
            Err(error) => {
                let message = mask_secrets(&error_message(&error));
                self.set_session(
                    &config.name,
                    SessionState {
                        status: "error".to_owned(),
                        tools: Vec::new(),
                        error: Some(message.clone()),
                    },
                );
                McpConnectResult {
                    name: config.name.clone(),
                    status: "error".to_owned(),
                    tools_count: 0,
                    error: Some(message),
                }
            }
        }
    }
}

#[async_trait]
impl McpAdmin for CliMcpAdmin {
    async fn list_servers(&self) -> Result<McpServerListResult, String> {
        let configs = self.store.read()?;
        Ok(McpServerListResult {
            servers: configs.iter().map(|config| self.record(config)).collect(),
        })
    }

    async fn add_server(
        &self,
        actor: &str,
        params: &McpAddServerParams,
    ) -> Result<McpServerAdminRecord, String> {
        validate_name(&params.name)?;
        validate_transport(&params.transport)?;
        validate_description(&params.description)?;
        let mut configs = self.store.read()?;
        if configs.iter().any(|config| config.name == params.name) {
            self.audit(actor, "add", &params.name, "duplicate");
            return Err(format!("server {:?} already exists", params.name));
        }
        let config = McpServerConfig::from_add(params);
        configs.push(config.clone());
        self.store.write(&configs)?;
        self.audit(actor, "add", &params.name, "ok");
        Ok(self.record(&config))
    }

    async fn update_server(
        &self,
        actor: &str,
        params: &McpUpdateServerParams,
    ) -> Result<McpServerAdminRecord, String> {
        validate_name(&params.name)?;
        if let Some(transport) = &params.transport {
            validate_transport(transport)?;
        }
        if let Some(description) = &params.description {
            validate_description(description)?;
        }
        let mut configs = self.store.read()?;
        let Some(index) = configs.iter().position(|config| config.name == params.name) else {
            self.audit(actor, "update", &params.name, "not_found");
            return Err(format!("server {:?} not found", params.name));
        };
        configs[index].apply_update(params);
        let config = configs[index].clone();
        self.store.write(&configs)?;
        // A config change invalidates the cached session.
        self.drop_session(&params.name);
        self.audit(actor, "update", &params.name, "ok");
        Ok(self.record(&config))
    }

    async fn remove_server(&self, actor: &str, name: &str) -> Result<(), String> {
        validate_name(name)?;
        let mut configs = self.store.read()?;
        let before = configs.len();
        configs.retain(|config| config.name != name);
        if configs.len() == before {
            self.audit(actor, "remove", name, "not_found");
            return Err(format!("server {name:?} not found"));
        }
        self.store.write(&configs)?;
        self.drop_session(name);
        self.audit(actor, "remove", name, "ok");
        Ok(())
    }

    async fn connect(&self, actor: &str, name: &str) -> Result<McpConnectResult, String> {
        validate_name(name)?;
        let configs = self.store.read()?;
        let Some(config) = configs.iter().find(|config| config.name == name) else {
            self.audit(actor, "connect", name, "not_found");
            return Err(format!("server {name:?} not found"));
        };
        let result = self.connect_config(config).await;
        self.audit(actor, "connect", name, &result.status);
        Ok(result)
    }

    async fn disconnect(&self, actor: &str, name: &str) -> Result<McpConnectResult, String> {
        validate_name(name)?;
        let configs = self.store.read()?;
        if !configs.iter().any(|config| config.name == name) {
            self.audit(actor, "disconnect", name, "not_found");
            return Err(format!("server {name:?} not found"));
        }
        self.set_session(name, SessionState::disconnected());
        self.audit(actor, "disconnect", name, "ok");
        Ok(McpConnectResult {
            name: name.to_owned(),
            status: "disconnected".to_owned(),
            tools_count: 0,
            error: None,
        })
    }

    async fn health(&self, name: &str) -> Result<McpHealthResult, String> {
        validate_name(name)?;
        let configs = self.store.read()?;
        let Some(config) = configs.iter().find(|config| config.name == name) else {
            return Err(format!("server {name:?} not found"));
        };
        let client = self.client(config)?;
        let healthy = client.list_tools().await.is_ok();
        Ok(McpHealthResult {
            name: name.to_owned(),
            healthy,
        })
    }

    async fn list_tools(&self) -> Result<McpToolListResult, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "mcp session registry poisoned".to_owned())?
            .clone();
        let mut tools = Vec::new();
        for (name, session) in &sessions {
            if session.status != "connected" {
                continue;
            }
            for tool in &session.tools {
                tools.push(tool_record(name, tool));
            }
        }
        tools.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
        Ok(McpToolListResult { tools })
    }

    async fn reconnect_all(&self, actor: &str) -> Result<McpServerListResult, String> {
        let configs = self.store.read()?;
        for config in &configs {
            if config.enabled {
                let _ = self.connect_config(config).await;
            } else {
                self.set_session(&config.name, SessionState::disconnected());
            }
        }
        self.audit(actor, "reconnect_all", "*", "ok");
        Ok(McpServerListResult {
            servers: configs.iter().map(|config| self.record(config)).collect(),
        })
    }
}

fn tool_record(server: &str, tool: &McpTool) -> McpToolRecord {
    McpToolRecord {
        name: tool.name.clone(),
        qualified_name: qualified_name(server, &tool.name),
        description: tool.description.clone().unwrap_or_default(),
        server_name: server.to_owned(),
        input_schema: tool.input_schema.clone(),
    }
}

/// `mcp_{server}_{tool}` when short and safe, else a stable hashed fallback.
fn qualified_name(server: &str, tool: &str) -> String {
    let raw = format!("mcp_{server}_{tool}");
    if raw.len() <= 64
        && raw.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return raw;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(raw.as_bytes());
    format!("mcpx_{:x}", digest)[..45].to_owned()
}

/// Python `^[a-z0-9]+(?:[-_][a-z0-9]+)*$` with a 64-char bound.
fn validate_name(name: &str) -> Result<(), String> {
    if name.len() > 64 {
        return Err("server name must be at most 64 characters".to_owned());
    }
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err("server name must not be empty".to_owned());
    }
    let mut previous_separator = true;
    for byte in bytes {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_separator = false,
            b'-' | b'_' if !previous_separator => previous_separator = true,
            _ => return Err(format!("invalid server name {name:?}")),
        }
    }
    if previous_separator {
        return Err(format!("invalid server name {name:?}"));
    }
    Ok(())
}

fn validate_transport(transport: &str) -> Result<(), String> {
    if matches!(transport, "stdio" | "http") {
        Ok(())
    } else {
        Err(format!("invalid transport {transport:?}"))
    }
}

/// Python bounds an MCP server description at 500 characters.
fn validate_description(description: &str) -> Result<(), String> {
    if description.len() > 500 {
        Err("server description must be at most 500 characters".to_owned())
    } else {
        Ok(())
    }
}

/// Replace `${VAR}` placeholders with process environment values (Python
/// `_interpolate_env`). A missing variable becomes the empty string.
fn interpolate(value: &str) -> String {
    if !value.contains("${") {
        return value.to_owned();
    }
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                output.push_str(&std::env::var(name).unwrap_or_default());
                rest = &after[end + 1..];
            }
            None => {
                output.push_str(&rest[start..]);
                return output;
            }
        }
    }
    output.push_str(rest);
    output
}

fn error_message(error: &McpError) -> String {
    error.to_string()
}

fn now_string() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    cool_store::python_datetime(seconds, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_matches_python() {
        for valid in ["filesystem", "my-server", "fs_tools", "a1-b2_c3"] {
            assert!(validate_name(valid).is_ok(), "{valid} should be valid");
        }
        for invalid in [
            "",
            "Upper",
            "trailing-",
            "-leading",
            "double--sep",
            "sp ace",
            "a.b",
        ] {
            assert!(
                validate_name(invalid).is_err(),
                "{invalid} should be invalid"
            );
        }
    }

    #[test]
    fn env_interpolation_replaces_known_and_missing_vars() {
        assert_eq!(interpolate("plain"), "plain");
        assert_eq!(interpolate("${COOL_TEST_MCP_MISSING_VAR}"), "");
        assert_eq!(interpolate("${unterminated"), "${unterminated");
        // A present variable is replaced with its value (`PATH` is always set).
        let path = std::env::var("PATH").unwrap_or_default();
        assert_eq!(interpolate("${PATH}"), path);
    }

    #[test]
    fn config_store_round_trips_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = McpConfigStore::open(dir.path().join(CONFIG_FILE));
        assert!(store.read().unwrap().is_empty());
        let config = McpServerConfig {
            name: "demo".to_owned(),
            transport: "stdio".to_owned(),
            command: "echo".to_owned(),
            enabled: true,
            timeout_s: 30.0,
            ..McpServerConfig::default()
        };
        store.write(std::slice::from_ref(&config)).unwrap();
        assert_eq!(store.read().unwrap(), vec![config]);
        std::fs::write(dir.path().join(CONFIG_FILE), "{not json").unwrap();
        assert!(store.read().is_err());
    }

    #[test]
    fn qualified_names_are_stable_and_bounded() {
        assert_eq!(qualified_name("filesystem", "read"), "mcp_filesystem_read");
        let long = qualified_name("a".repeat(80).as_str(), "b");
        assert!(long.starts_with("mcpx_"));
        assert!(long.len() <= 45);
    }
}
