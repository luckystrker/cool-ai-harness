//! Application settings (M11 WS1c).
//!
//! The Rust runtime keeps its process-local defaults out of the protocol: the
//! mutable settings that the web UI owns live in a small config document on the
//! data root and are read/written through these commands. Today the only setting
//! is the default system prompt applied to a run that did not supply one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::IdempotencyKey;

/// The effective default system prompt and where it comes from. `source` is
/// `inline` when an operator-set prompt is stored, otherwise `builtin` (Rust
/// ships no bundled prompt file, so an unset default is empty and no system
/// message is sent).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SystemPromptRecord {
    pub prompt: String,
    pub is_custom: bool,
    pub source: String,
}

/// Set the default system prompt. An empty (or whitespace-only) prompt clears it
/// and resets the effective state to the built-in default.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SystemPromptSetParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub prompt: String,
}
