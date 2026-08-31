use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type SpikeResult<T> = Result<T, SpikeError>;

#[derive(Debug)]
pub enum SpikeError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Sql(rusqlite::Error),
    InvalidTransition { from: String, to: String },
    Protocol(String),
    MethodNotFound(String),
    IdempotencyConflict,
    EffectConflict,
    ForeignDatabase,
    FrameTooLarge,
    TooManyMessages,
    InjectedFailure(String),
    StaleApproval,
    Worker(String),
}

impl Display for SpikeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Sql(error) => write!(formatter, "SQLite error: {error}"),
            Self::InvalidTransition { from, to } => {
                write!(formatter, "invalid run transition {from} -> {to}")
            }
            Self::Protocol(message) => write!(formatter, "protocol error: {message}"),
            Self::MethodNotFound(method) => write!(formatter, "method not found: {method}"),
            Self::IdempotencyConflict => write!(formatter, "idempotency key payload conflict"),
            Self::EffectConflict => write!(formatter, "tool effect identity conflict"),
            Self::ForeignDatabase => write!(formatter, "database is not an M0 spike database"),
            Self::FrameTooLarge => write!(formatter, "frame exceeds the configured limit"),
            Self::TooManyMessages => write!(formatter, "worker message limit exceeded"),
            Self::InjectedFailure(name) => write!(formatter, "injected failure: {name}"),
            Self::StaleApproval => write!(formatter, "approval is stale or already resolved"),
            Self::Worker(message) => write!(formatter, "worker error: {message}"),
        }
    }
}

impl std::error::Error for SpikeError {}

impl From<std::io::Error> for SpikeError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for SpikeError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<rusqlite::Error> for SpikeError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    pub run_id: String,
    pub status: String,
    pub approval_id: Option<String>,
    pub approval_revision: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub event_id: String,
    pub schema_version: u32,
    pub session_id: String,
    pub run_id: String,
    pub item_id: Option<String>,
    pub seq: i64,
    pub occurred_at: i64,
    pub actor: String,
    pub source: String,
    pub causation_id: Option<String>,
    pub correlation_id: Option<String>,
    pub kind: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: String,
    pub session_id: String,
    pub actor: String,
    pub status: String,
    pub next_seq: i64,
    pub worker_attempts: i64,
    pub tool_effect_count: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientState {
    pub run_status: String,
    pub content: String,
    pub approval_status: Option<String>,
    pub tool_effect_count: u32,
    pub last_seq: Option<i64>,
    #[serde(skip)]
    seen_event_ids: BTreeSet<String>,
}

impl ClientState {
    pub fn apply(&mut self, event: &Event) {
        if !self.seen_event_ids.insert(event.event_id.clone()) {
            return;
        }
        self.last_seq = Some(event.seq);
        match event.kind.as_str() {
            "run.started" => self.run_status = "running".to_owned(),
            "content.delta" => {
                if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                    self.content.push_str(text);
                }
            }
            "tool.approval_required" => {
                self.run_status = "awaiting_approval".to_owned();
                self.approval_status = Some("pending".to_owned());
            }
            "approval.resolved" => {
                self.approval_status = event
                    .payload
                    .get("decision")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "tool.completed" => self.tool_effect_count += 1,
            "run.completed" => self.run_status = "completed".to_owned(),
            "run.failed" => self.run_status = "failed".to_owned(),
            "run.cancelled" => self.run_status = "cancelled".to_owned(),
            _ => {}
        }
    }

    pub fn replay(events: &[Event]) -> Self {
        let mut state = Self::default();
        for event in events {
            state.apply(event);
        }
        state
    }
}
