//! Global skills admin (M11 WS1d / Workstream B2).
//!
//! Skills bundled by enabled plugins are projected read-only by
//! `extensions.status`. This family is the operator-owned skills store the web
//! admin manages: a `SKILL.md` directory tree on the data root. A skill is
//! instructions, never trusted code; nothing here executes a skill body.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::IdempotencyKey;

/// One skill in the global skills store.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillAdminRecord {
    pub name: String,
    pub description: String,
    /// Where the skill was loaded from (`user` for the Rust store).
    pub source: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    pub version: String,
    #[serde(default)]
    pub body: String,
}

/// All skills in the global store.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillListResult {
    #[serde(default)]
    pub skills: Vec<SkillAdminRecord>,
}

/// List skills, optionally filtered by source.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillListParams {
    #[serde(default)]
    pub source: Option<String>,
}

/// Create a new skill (`SKILL.md`) in the global store.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    pub body: String,
    /// Accepted for parity with the Python API (`global`/`user`); the Rust store
    /// is a single directory and reports `user`.
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "global".to_owned()
}

/// Result of creating a skill.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillCreateResult {
    pub name: String,
    pub path: String,
    pub scope: String,
}

/// Delete a skill from the global store by name.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkillDeleteParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub name: String,
}
