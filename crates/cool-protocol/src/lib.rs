use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const PROTOCOL_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;

pub type Extensions = BTreeMap<String, Value>;

macro_rules! fixed_wire_string {
    ($name:ident, $value:literal, $typescript:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, TS)]
        #[ts(type = $typescript)]
        pub struct $name;

        impl $name {
            pub const VALUE: Self = Self;
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str($value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let actual = String::deserialize(deserializer)?;
                if actual == $value {
                    Ok(Self)
                } else {
                    Err(serde::de::Error::custom(concat!("expected literal ", $value)))
                }
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({"type": "string", "const": $value})
            }
        }
    };
}

fixed_wire_string!(JsonRpcV2, "2.0", "\"2.0\"");
fixed_wire_string!(CoolCommandMethod, "cool.command", "\"cool.command\"");
fixed_wire_string!(RunEventMethod, "run.event", "\"run.event\"");

#[derive(Clone, Copy, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct V1Version(#[schemars(range(min = 1, max = 1))] u32);

impl V1Version {
    pub const VALUE: Self = Self(1);
}

impl<'de> Deserialize<'de> for V1Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == 1 {
            Ok(Self::VALUE)
        } else {
            Err(serde::de::Error::custom(
                "only App Protocol version 1 is supported",
            ))
        }
    }
}

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(#[schemars(length(min = 1))] String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() {
            Err("idempotency key must not be empty")
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ActorRef {
    pub id: String,
    pub kind: ActorKind,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ActorKind {
    LocalUser,
    ServerUser,
    TelegramUser,
    System,
    Worker,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CommandEnvelope {
    #[ts(type = "1")]
    pub protocol_version: V1Version,
    pub command_id: String,
    pub command: Command,
}

/// A server-normalized command. Transport authentication supplies `actor`;
/// it is intentionally absent from the client-deserializable envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorizedCommand {
    pub actor: ActorRef,
    pub command: CommandEnvelope,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(tag = "method", content = "params", deny_unknown_fields)]
#[ts(export)]
pub enum Command {
    #[serde(rename = "initialize")]
    Initialize(InitializeParams),
    #[serde(rename = "session.create")]
    SessionCreate(SessionCreateParams),
    #[serde(rename = "session.load")]
    SessionLoad(SessionLoadParams),
    #[serde(rename = "session.prompt")]
    SessionPrompt(SessionPromptParams),
    #[serde(rename = "run.cancel")]
    RunCancel(RunCancelParams),
    #[serde(rename = "run.events")]
    RunEvents(RunEventsParams),
    #[serde(rename = "approval.resolve")]
    ApprovalResolve(ApprovalResolveParams),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct InitializeParams {
    pub client_name: String,
    pub client_version: String,
    pub supported_protocol_versions: Vec<u32>,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub title: Option<String>,
    pub project_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionLoadParams {
    pub session_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionPromptParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub session_id: String,
    pub content: Vec<ContentPart>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum ContentPart {
    Text {
        text: String,
    },
    Artifact {
        #[serde(rename = "artifactId")]
        #[ts(rename = "artifactId")]
        artifact_id: String,
    },
    Image {
        #[serde(rename = "artifactId")]
        #[ts(rename = "artifactId")]
        artifact_id: String,
        #[serde(rename = "mediaType")]
        #[ts(rename = "mediaType")]
        media_type: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunCancelParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub run_id: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunEventsParams {
    pub run_id: String,
    #[ts(type = "number | null")]
    pub after_seq: Option<u64>,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalResolveParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub approval_id: String,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub decision: ApprovalDecision,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EventCursor {
    pub run_id: String,
    #[ts(type = "number | null")]
    pub after_seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EventPage {
    pub events: Vec<EventEnvelope>,
    pub next_cursor: Option<EventCursor>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProtocolError {
    pub rpc_code: i32,
    pub cool_code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub safe_details: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EventEnvelope {
    pub event_id: String,
    #[ts(type = "1")]
    pub schema_version: V1Version,
    pub session_id: String,
    pub run_id: String,
    pub item_id: Option<String>,
    #[ts(type = "number")]
    pub seq: u64,
    pub occurred_at: String,
    pub actor: ActorRef,
    pub source: String,
    pub causation_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event: CanonicalEvent,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: Extensions,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(tag = "kind", content = "payload", deny_unknown_fields)]
#[ts(export)]
pub enum CanonicalEvent {
    #[serde(rename = "session.created")]
    SessionCreated(SessionEvent),
    #[serde(rename = "session.updated")]
    SessionUpdated(SessionEvent),
    #[serde(rename = "session.compacted")]
    SessionCompacted(SessionCompacted),
    #[serde(rename = "run.started")]
    RunStarted(RunStarted),
    #[serde(rename = "run.completed")]
    RunCompleted(RunTerminal),
    #[serde(rename = "run.failed")]
    RunFailed(RunTerminal),
    #[serde(rename = "run.cancelled")]
    RunCancelled(RunTerminal),
    #[serde(rename = "item.started")]
    ItemStarted(ItemEvent),
    #[serde(rename = "item.updated")]
    ItemUpdated(ItemEvent),
    #[serde(rename = "item.completed")]
    ItemCompleted(ItemEvent),
    #[serde(rename = "content.delta")]
    ContentDelta(TextDelta),
    #[serde(rename = "reasoning.delta")]
    ReasoningDelta(TextDelta),
    #[serde(rename = "tool.requested")]
    ToolRequested(ToolRequested),
    #[serde(rename = "tool.approval_required")]
    ToolApprovalRequired(ToolApprovalRequired),
    #[serde(rename = "tool.approval_resolved")]
    ToolApprovalResolved(ToolApprovalResolved),
    #[serde(rename = "tool.started")]
    ToolStarted(ToolLifecycle),
    #[serde(rename = "tool.completed")]
    ToolCompleted(ToolCompleted),
    #[serde(rename = "tool.failed")]
    ToolFailed(ToolFailed),
    #[serde(rename = "plan.created")]
    PlanCreated(PlanCreated),
    #[serde(rename = "plan.step_started")]
    PlanStepStarted(PlanStep),
    #[serde(rename = "plan.step_completed")]
    PlanStepCompleted(PlanStep),
    #[serde(rename = "plan.progress")]
    PlanProgress(PlanProgress),
    #[serde(rename = "artifact.created")]
    ArtifactCreated(ArtifactCreated),
    #[serde(rename = "usage.updated")]
    UsageUpdated(UsageUpdated),
    #[serde(rename = "budget.warning")]
    BudgetWarning(BudgetEvent),
    #[serde(rename = "budget.exceeded")]
    BudgetExceeded(BudgetEvent),
    #[serde(rename = "subagent.started")]
    SubagentStarted(SubagentEvent),
    #[serde(rename = "subagent.progress")]
    SubagentProgress(SubagentProgress),
    #[serde(rename = "subagent.completed")]
    SubagentCompleted(SubagentEvent),
    #[serde(rename = "subagent.failed")]
    SubagentFailed(SubagentEvent),
    #[serde(rename = "worker.started")]
    WorkerStarted(WorkerEvent),
    #[serde(rename = "worker.failed")]
    WorkerFailed(WorkerEvent),
    #[serde(rename = "worker.restarted")]
    WorkerRestarted(WorkerEvent),
    #[serde(rename = "research.stage")]
    ResearchStage(ResearchStage),
    #[serde(rename = "research.started")]
    ResearchStarted(ResearchStarted),
    #[serde(rename = "research.source_found")]
    ResearchSourceFound(ResearchSource),
    #[serde(rename = "research.subquestion_started")]
    ResearchSubquestionStarted(ResearchSubquestion),
    #[serde(rename = "research.subquestion_completed")]
    ResearchSubquestionCompleted(ResearchSubquestion),
    #[serde(rename = "research.completed")]
    ResearchCompleted(ResearchTerminal),
    #[serde(rename = "research.failed")]
    ResearchFailed(ResearchTerminal),
    #[serde(rename = "research.cancelled")]
    ResearchCancelled(ResearchTerminal),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionEvent {
    pub title: Option<String>,
    pub project_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionCompacted {
    pub retained_items: u32,
    pub summary_item_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunStarted {
    pub model: Option<String>,
    pub mode: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunTerminal {
    pub reason: String,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ItemEvent {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolRequested>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TextDelta {
    pub text: String,
    pub channel: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolRequested {
    pub call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolApprovalRequired {
    pub call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
    pub reason: String,
    pub approval_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub breakpoint_type: Option<String>,
    pub result_preview: Option<String>,
    pub current_content: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolApprovalResolved {
    pub call_id: String,
    pub approval_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub decision: ApprovalOutcome,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolLifecycle {
    pub call_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolCompleted {
    pub call_id: String,
    pub name: String,
    pub result: Value,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolFailed {
    pub call_id: String,
    pub name: String,
    pub error_code: String,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanCreated {
    pub plan_id: String,
    pub title: Option<String>,
    pub total_steps: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanStep {
    pub plan_id: String,
    pub position: u32,
    pub title: String,
    pub status: String,
    pub result_summary: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanProgressStatus {
    Executing,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanProgress {
    pub plan_id: String,
    pub completed_steps: u32,
    pub total_steps: u32,
    pub message: Option<String>,
    pub status: PlanProgressStatus,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ArtifactCreated {
    pub artifact_id: String,
    pub kind: String,
    pub name: String,
    pub media_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct UsageUpdated {
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    #[ts(type = "number")]
    pub total_tokens: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BudgetEvent {
    pub window: String,
    pub spend_usd: f64,
    pub limit_usd: f64,
    pub percent: f64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentEvent {
    pub subagent_run_id: String,
    pub name: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentProgress {
    pub subagent_run_id: String,
    pub message: String,
    pub percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkerEvent {
    pub worker_id: String,
    pub attempt: u32,
    pub code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchStage {
    pub stage: String,
    pub message: Option<String>,
    pub progress: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchStarted {
    pub research_run_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchSource {
    pub url: String,
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchSubquestion {
    pub index: u32,
    pub question: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchTerminal {
    pub artifact_id: Option<String>,
    pub source_count: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ClientState {
    pub run_status: Option<String>,
    pub content: String,
    pub reasoning: String,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    #[serde(default)]
    pub approvals: BTreeMap<String, String>,
    pub active_plan_id: Option<String>,
    pub plan_status: Option<String>,
    #[serde(default)]
    pub plan_steps: BTreeMap<String, String>,
    #[serde(default)]
    pub plan_completed_steps: u32,
    #[serde(default)]
    pub plan_total_steps: u32,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub subagents: BTreeMap<String, String>,
    #[serde(default)]
    pub workers: BTreeMap<String, String>,
    pub budget_status: Option<String>,
    pub research_status: Option<String>,
    #[ts(type = "number | null")]
    pub last_seq: Option<u64>,
    #[serde(skip)]
    #[ts(skip)]
    seen_events: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayError(pub String);

impl std::fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReplayError {}

impl ClientState {
    pub fn try_apply(&mut self, envelope: &EventEnvelope) -> Result<(), ReplayError> {
        let fingerprint = serde_json::to_string(envelope)
            .map_err(|error| ReplayError(format!("event serialization failed: {error}")))?;
        if let Some(previous) = self.seen_events.get(&envelope.event_id) {
            if previous == &fingerprint {
                return Ok(());
            }
            return Err(ReplayError(format!(
                "event id {} was reused with different content",
                envelope.event_id
            )));
        }
        let event_plan_id = match &envelope.event {
            CanonicalEvent::PlanCreated(payload) => Some(payload.plan_id.as_str()),
            CanonicalEvent::PlanStepStarted(payload) => Some(payload.plan_id.as_str()),
            CanonicalEvent::PlanStepCompleted(payload) => Some(payload.plan_id.as_str()),
            CanonicalEvent::PlanProgress(payload) => Some(payload.plan_id.as_str()),
            _ => None,
        };
        if let (Some(active), Some(incoming)) = (&self.active_plan_id, event_plan_id)
            && active != incoming
        {
            return Err(ReplayError(format!(
                "plan id mismatch: active {active}, got {incoming}"
            )));
        }
        if let Some(last_seq) = self.last_seq {
            if envelope.seq <= last_seq {
                return Err(ReplayError(format!(
                    "stale or conflicting sequence {} after {last_seq}",
                    envelope.seq
                )));
            }
            if envelope.seq != last_seq + 1 {
                return Err(ReplayError(format!(
                    "sequence gap: expected {}, got {}",
                    last_seq + 1,
                    envelope.seq
                )));
            }
        } else if envelope.seq != 1 {
            return Err(ReplayError(format!(
                "sequence must start at 1, got {}",
                envelope.seq
            )));
        }
        self.seen_events
            .insert(envelope.event_id.clone(), fingerprint);
        self.last_seq = Some(envelope.seq);
        match &envelope.event {
            CanonicalEvent::RunStarted(_) => self.run_status = Some("running".to_owned()),
            CanonicalEvent::RunCompleted(_) => self.run_status = Some("completed".to_owned()),
            CanonicalEvent::RunFailed(_) => self.run_status = Some("failed".to_owned()),
            CanonicalEvent::RunCancelled(_) => self.run_status = Some("cancelled".to_owned()),
            CanonicalEvent::ContentDelta(payload) => self.content.push_str(&payload.text),
            CanonicalEvent::ReasoningDelta(payload) => self.reasoning.push_str(&payload.text),
            CanonicalEvent::ToolRequested(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "requested".to_owned());
            }
            CanonicalEvent::ToolApprovalRequired(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "awaiting_approval".to_owned());
                self.approvals
                    .insert(payload.approval_id.clone(), "pending".to_owned());
            }
            CanonicalEvent::ToolApprovalResolved(payload) => {
                self.approvals.insert(
                    payload.approval_id.clone(),
                    match payload.decision {
                        ApprovalOutcome::Approved => "approved",
                        ApprovalOutcome::Denied => "denied",
                        ApprovalOutcome::TimedOut => "timed_out",
                    }
                    .to_owned(),
                );
            }
            CanonicalEvent::ToolStarted(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "running".to_owned());
            }
            CanonicalEvent::ToolCompleted(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "completed".to_owned());
            }
            CanonicalEvent::ToolFailed(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "failed".to_owned());
            }
            CanonicalEvent::PlanCreated(payload) => {
                self.active_plan_id = Some(payload.plan_id.clone());
                self.plan_status = Some("planned".to_owned());
                self.plan_completed_steps = 0;
                self.plan_total_steps = payload.total_steps;
                self.plan_steps.clear();
            }
            CanonicalEvent::PlanStepStarted(payload) => {
                self.active_plan_id
                    .get_or_insert_with(|| payload.plan_id.clone());
                self.plan_status = Some("running".to_owned());
                self.plan_steps
                    .insert(payload.position.to_string(), "running".to_owned());
            }
            CanonicalEvent::PlanStepCompleted(payload) => {
                self.active_plan_id
                    .get_or_insert_with(|| payload.plan_id.clone());
                self.plan_steps
                    .insert(payload.position.to_string(), payload.status.clone());
            }
            CanonicalEvent::PlanProgress(payload) => {
                self.active_plan_id
                    .get_or_insert_with(|| payload.plan_id.clone());
                self.plan_completed_steps = payload.completed_steps;
                self.plan_total_steps = payload.total_steps;
                self.plan_status = Some(
                    match payload.status {
                        PlanProgressStatus::Executing => "running",
                        PlanProgressStatus::Completed => "completed",
                        PlanProgressStatus::Failed => "failed",
                    }
                    .to_owned(),
                );
            }
            CanonicalEvent::ArtifactCreated(payload) => {
                if !self.artifacts.contains(&payload.artifact_id) {
                    self.artifacts.push(payload.artifact_id.clone());
                }
            }
            CanonicalEvent::BudgetWarning(_) => {
                self.budget_status = Some("warning".to_owned());
            }
            CanonicalEvent::BudgetExceeded(_) => {
                self.budget_status = Some("exceeded".to_owned());
            }
            CanonicalEvent::SubagentStarted(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "running".to_owned());
            }
            CanonicalEvent::SubagentProgress(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "running".to_owned());
            }
            CanonicalEvent::SubagentCompleted(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "completed".to_owned());
            }
            CanonicalEvent::SubagentFailed(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "failed".to_owned());
            }
            CanonicalEvent::WorkerStarted(payload) => {
                self.workers
                    .insert(payload.worker_id.clone(), "running".to_owned());
            }
            CanonicalEvent::WorkerFailed(payload) => {
                self.workers
                    .insert(payload.worker_id.clone(), "failed".to_owned());
            }
            CanonicalEvent::WorkerRestarted(payload) => {
                self.workers
                    .insert(payload.worker_id.clone(), "running".to_owned());
            }
            CanonicalEvent::ResearchStarted(_) | CanonicalEvent::ResearchStage(_) => {
                self.research_status = Some("running".to_owned());
            }
            CanonicalEvent::ResearchCompleted(_) => {
                self.research_status = Some("completed".to_owned());
            }
            CanonicalEvent::ResearchFailed(_) => {
                self.research_status = Some("failed".to_owned());
            }
            CanonicalEvent::ResearchCancelled(_) => {
                self.research_status = Some("cancelled".to_owned());
            }
            _ => {}
        }
        Ok(())
    }

    pub fn try_replay(events: &[EventEnvelope]) -> Result<Self, ReplayError> {
        let mut state = Self::default();
        let mut ordered = events.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|event| event.seq);
        let run_id = ordered.first().map(|event| event.run_id.as_str());
        for event in ordered {
            if Some(event.run_id.as_str()) != run_id {
                return Err(ReplayError("a client state cannot mix run ids".to_owned()));
            }
            state.try_apply(event)?;
        }
        Ok(state)
    }

    pub fn replay(events: &[EventEnvelope]) -> Self {
        Self::try_replay(events).expect("valid canonical event sequence")
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GoldenTrace {
    pub name: String,
    pub events: Vec<EventEnvelope>,
    pub expected_state: ClientState,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export)]
pub enum StreamFrame {
    Event(Box<EventEnvelope>),
    Keepalive(StreamKeepalive),
    End(StreamEnd),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum RpcId {
    String(#[schemars(length(max = 128))] String),
    Integer(#[ts(type = "number")] i64),
    Null,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcRequest {
    pub jsonrpc: JsonRpcV2,
    pub id: RpcId,
    pub method: CoolCommandMethod,
    pub params: CommandEnvelope,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TransportLimits {
    pub max_frame_bytes: u32,
    pub max_rpc_id_bytes: u16,
    pub max_in_flight: u16,
    pub outbound_queue: u16,
    pub event_page_limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct InitializeResult {
    #[ts(type = "1")]
    pub protocol_version: V1Version,
    pub server_name: String,
    pub server_version: String,
    pub capabilities: BTreeSet<String>,
    pub limits: TransportLimits,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionCreatedResult {
    pub session_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionLoadedResult {
    pub session_id: String,
    pub active_run_id: Option<String>,
    #[ts(type = "number | null")]
    pub last_seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PromptAcceptedResult {
    pub run_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunCancelledResult {
    pub run_id: String,
    pub accepted: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalResolvedResult {
    pub approval_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub outcome: ApprovalOutcome,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export)]
pub enum ResponsePayload {
    Initialized(InitializeResult),
    SessionCreated(SessionCreatedResult),
    SessionLoaded(SessionLoadedResult),
    PromptAccepted(PromptAcceptedResult),
    RunCancelled(RunCancelledResult),
    ApprovalResolved(ApprovalResolvedResult),
    EventPage(EventPage),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcSuccess {
    pub jsonrpc: JsonRpcV2,
    pub id: RpcId,
    pub result: ResponsePayload,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcFailure {
    pub jsonrpc: JsonRpcV2,
    pub id: RpcId,
    pub error: ProtocolError,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcNotification {
    pub jsonrpc: JsonRpcV2,
    pub method: RunEventMethod,
    pub params: StreamFrame,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum ServerFrame {
    Success(RpcSuccess),
    Failure(RpcFailure),
    Notification(RpcNotification),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StreamKeepalive {
    pub run_id: String,
    #[ts(type = "number")]
    pub last_seq: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StreamEnd {
    pub run_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolSchemaDocument {
    pub command: CommandEnvelope,
    pub rpc_request: RpcRequest,
    pub server_frame: ServerFrame,
    pub event: EventEnvelope,
    pub event_page: EventPage,
    pub error: ProtocolError,
    pub golden_trace: GoldenTrace,
    pub stream_frame: StreamFrame,
}
