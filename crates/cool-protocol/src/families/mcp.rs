//! Global MCP server admin (M11 WS1d).
//!
//! The M8 extension runtime owns the MCP servers *bundled by plugins*
//! (`extensions.status` projects them read-only). This family is the separate
//! operator-owned global MCP configuration the web admin manages: server
//! configs, live connection status, discovered tools and the MCP registry
//! install path. Records are secret-free by construction: `env`/`headers`
//! values are accepted on writes but never projected back.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::IdempotencyKey;

/// One tool exposed by a configured MCP server.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpToolRecord {
    pub name: String,
    pub qualified_name: String,
    pub description: String,
    pub server_name: String,
    #[serde(default)]
    pub input_schema: Value,
}

/// One configured MCP server with its live status and discovered tools.
/// `env`/`headers`/`cwd` are deliberately absent (they can carry credentials).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpServerAdminRecord {
    pub name: String,
    /// `stdio` or `http`.
    pub transport: String,
    /// `disconnected`, `connecting`, `connected` or `error`.
    pub status: String,
    pub enabled: bool,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
    pub url: String,
    pub capabilities: Vec<String>,
    pub timeout_s: f64,
    pub version: String,
    pub author: String,
    pub compatibility: String,
    pub error: Option<String>,
    #[serde(default)]
    pub tools: Vec<McpToolRecord>,
    #[serde(default)]
    pub server_info: Option<Value>,
}

/// All configured MCP servers.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpServerListResult {
    #[serde(default)]
    pub servers: Vec<McpServerAdminRecord>,
}

/// All tools discovered across connected servers.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpToolListResult {
    #[serde(default)]
    pub tools: Vec<McpToolRecord>,
}

/// Result of a connect/disconnect/install action.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpConnectResult {
    pub name: String,
    pub status: String,
    pub tools_count: i64,
    pub error: Option<String>,
}

/// Result of a health probe.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpHealthResult {
    pub name: String,
    pub healthy: bool,
}

/// Add a global MCP server configuration.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpAddServerParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
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

fn default_transport() -> String {
    "stdio".to_owned()
}

fn default_enabled() -> bool {
    true
}

fn default_timeout_s() -> f64 {
    30.0
}

/// Partial update for a global MCP server configuration.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpUpdateServerParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    pub transport: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub url: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub timeout_s: Option<f64>,
}

/// Address one configured MCP server by name.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpServerNameParams {
    pub name: String,
}

/// Install an MCP server from the official registry by registry name.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpStoreInstallParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub registry_name: String,
    #[serde(default)]
    pub local_name: String,
}

/// One server returned by an MCP registry search.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpStoreItemRecord {
    pub name: String,
    pub description: String,
    pub version: String,
    pub repository_url: String,
    pub install_command: String,
    pub transport: String,
    pub packages_count: i64,
}

/// Registry search result plus the query that produced it.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpStoreSearchResult {
    #[serde(default)]
    pub results: Vec<McpStoreItemRecord>,
    pub query: String,
}

/// Registry search parameters.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpStoreSearchParams {
    #[serde(default)]
    pub query: String,
    pub limit: u16,
}

/// Registry "popular" parameters.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpStorePopularParams {
    pub limit: u16,
}
