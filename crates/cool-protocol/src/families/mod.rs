//! Typed App Protocol families for the legacy React surface (M10).
//!
//! These types are the protocol-owned mirror of the `cool-store` typed
//! records so `cool-protocol` stays independent of `rusqlite`. The App Server
//! converts between the two through a top-level snake_case/camelCase bridge:
//! JSON columns (`metadata`, `settings`, `arguments`, ...) are opaque payloads
//! and are never key-transformed.
//!
//! Mutating params carry an [`IdempotencyKey`](crate::IdempotencyKey); reads
//! carry bounded `limit`/`offset` fields. Server responses omit secrets
//! (provider API keys, webhook secrets) by construction.

mod content;
mod knowledge;
mod ops;

pub use content::*;
pub use knowledge::*;
pub use ops::*;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::IdempotencyKey;

/// Address one numeric legacy record.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct LegacyIdParams {
    pub id: i64,
}

/// Address one numeric legacy record for a mutation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct IdempotentIdParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub id: i64,
}

/// A mutation without a target record (for example clearing an override).
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct IdempotentParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
}

/// Parameters that carry no fields of their own.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EmptyParams {}

/// Result of a delete-style mutation: the removed record id.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct DeletedResult {
    pub deleted: i64,
}

/// Result of a void mutation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct LegacyOkResult {
    pub ok: bool,
}

/// Result of a bulk mutation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AffectedResult {
    pub affected: u64,
    pub action: String,
}
