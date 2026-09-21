//! Extension admin reads (M11 WS1/WS3).
//!
//! The M8 extension runtime owns plugin installs, hook declarations, skills and
//! the MCP servers bundled by enabled plugins. These records are a read-only,
//! secret-free projection of that state for the web admin surface. The runtime
//! is never a source of truth here: the app server reads whatever the installed
//! extension host reports and renders it; nothing in this module can widen a
//! capability or reach the database.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::IdempotencyKey;

/// One diagnostic emitted while loading an installed plugin.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionDiagnosticRecord {
    pub code: String,
    pub level: String,
    pub message: String,
    pub path: String,
}

/// One installed plugin as seen by the admin surface.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PluginRecord {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub source_type: String,
    pub source: String,
    pub revision: String,
    pub content_hash: String,
    pub installed_at: String,
    pub required_capabilities: Vec<String>,
    pub resolved_dependencies: Vec<String>,
    pub diagnostics: Vec<ExtensionDiagnosticRecord>,
    /// True only when the on-disk tree still matches the recorded content hash
    /// *and* its manifest identity. An enabled plugin with
    /// `contentVerified: false` is treated as failed by the runtime (its hooks,
    /// skills and MCP servers are not loaded) even though the install record
    /// remains visible for diagnosis.
    pub content_verified: bool,
}

/// One compatibility worker supervised by the extension runtime.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkerRecord {
    pub id: String,
    /// `running`, `failed` or `restarted`.
    pub status: String,
    pub attempt: i64,
    pub code: Option<String>,
}

/// One hook declaration from an enabled plugin plus its review state. `approved`
/// is true only when the stored review hash matches the hook's exact trust hash,
/// so changing the command/args/env/matcher resets approval.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct HookRecord {
    pub plugin: String,
    pub id: String,
    pub event: String,
    pub handler: String,
    pub order: i64,
    pub parallel: bool,
    pub capabilities: Vec<String>,
    pub trust_hash: String,
    pub approved: bool,
}

/// One skill parsed from an enabled plugin's `SKILL.md` tree.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillRecord {
    pub name: String,
    pub description: String,
    pub plugin: String,
    pub allowed_tools: Vec<String>,
}

/// One MCP server bundled by an enabled plugin.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpServerRecord {
    pub plugin: String,
    pub name: String,
    /// `stdio` or `streamable_http`.
    pub transport: String,
    pub endpoint: String,
}

/// The configured MCP tool policy. `enabled` is `None` when the policy keeps
/// every non-disabled tool; `disabled` always applies.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct McpToolPolicyRecord {
    pub enabled: Option<Vec<String>>,
    pub disabled: Vec<String>,
}

/// Read-only snapshot of the extension admin surface: installed plugins,
/// supervised workers, hook review state, discovered skills, plugin-bundled MCP
/// servers and the MCP tool policy.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExtensionStatusResult {
    #[serde(default)]
    pub plugins: Vec<PluginRecord>,
    #[serde(default)]
    pub workers: Vec<WorkerRecord>,
    #[serde(default)]
    pub hooks: Vec<HookRecord>,
    #[serde(default)]
    pub skills: Vec<SkillRecord>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerRecord>,
    #[serde(default)]
    pub mcp_tool_policy: McpToolPolicyRecord,
}

/// Enable or disable one installed plugin. Disabling never loads the tree;
/// enabling re-verifies the content hash and fails closed on a tampered tree.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PluginEnabledParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub plugin: String,
    pub enabled: bool,
}

/// Approve or reject one hook declaration. `trust_hash` must equal the hook's
/// current trust hash, so an operator cannot approve a definition they did not
/// review; a mismatch fails closed.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct HookReviewParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub plugin: String,
    pub hook: String,
    pub trust_hash: String,
    pub approved: bool,
}
