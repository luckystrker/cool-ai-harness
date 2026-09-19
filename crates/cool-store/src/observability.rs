//! Inspector projections built from the canonical event log (M10).
//!
//! Implements the M10 exit criterion "inspector builds timeline from event
//! log" by reconstructing the Python inspector's view of a run
//! (`backend/app/observability/inspector.py`) directly from `run_events`,
//! without reading messages, tool-call rows or approval audits.
//!
//! # Projection semantics
//!
//! * The Python inspector groups the seq-ordered event log into per-iteration
//!   `IterationInfo` records, using `llm_call_complete` as iteration
//!   boundaries. Rust keeps the raw seq-ordered event stream as
//!   [`TimelineEntry`] values and classifies each one into a `phase`, so the
//!   same grouping is recoverable (and is asserted against the committed
//!   `tests/fixtures/timeline_parity.json`).
//! * `total_duration_ms` mirrors Python exactly: the sum of the
//!   `llm_call_complete.duration_ms` values, falling back to the last
//!   `finish.elapsed_ms` when that sum is zero (`None` when it stays zero).
//! * [`ComparisonDeltas::duration_ms`] mirrors Python's `delta_duration_ms`,
//!   which is the *wall-clock* difference derived from
//!   `started_at`/`finished_at` (not the event-derived total). Timestamps are
//!   parsed with [`crate::time::parse_python_datetime`], so deltas have
//!   whole-second resolution while the original strings are preserved in the
//!   output.
//! * Python never merges messages/tool-call rows/approval audits into the
//!   timeline; Rust does the same. `event_count` and `tool_calls` in the
//!   comparison deltas are Rust-only additions with no Python counterpart.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::LegacyStore;
use crate::domains::runs::{AgentRun, RunEvent};
use crate::error::StoreError;
use crate::time::parse_python_datetime;

/// One step of a reconstructed run timeline, in event `seq` order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    /// Zero-based position in the seq-ordered event stream.
    pub index: usize,
    /// Raw `run_events.kind` (for example `llm_call_complete`).
    pub kind: String,
    /// Coarse phase used to group related events (`llm`, `tools`, ...).
    pub phase: String,
    /// Derived terminal/decision status, when the event carries one.
    pub status: Option<String>,
    /// Short human label (model, tool or reason), when available.
    pub title: Option<String>,
    /// Original `run_events.created_at` string, preserved verbatim.
    pub occurred_at: String,
    /// Event duration in milliseconds, when the payload carries one.
    pub duration_ms: Option<i64>,
    /// Verbatim `run_events.payload`.
    pub payload: Option<Value>,
}

/// Full inspector timeline for a single run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTimeline {
    pub run_id: i64,
    pub conversation_id: i64,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Event-derived total, matching Python `total_duration_ms`.
    pub total_duration_ms: Option<i64>,
    pub entries: Vec<TimelineEntry>,
    pub usage: Option<Value>,
    pub error: Option<String>,
}

/// Metric deltas between two runs (the difference is always `right - left`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonDeltas {
    /// Wall-clock duration delta, matching Python `delta_duration_ms`.
    pub duration_ms: Option<i64>,
    /// Rust-only: difference in projected event count.
    pub event_count: i64,
    /// Difference in `usage.total_tokens` (missing usage counts as zero).
    pub total_tokens: Option<i64>,
    /// Difference in `usage.cost_usd`, `None` unless both runs report it.
    pub cost_usd: Option<f64>,
    /// Rust-only: difference in `tool_call_start` event count.
    pub tool_calls: i64,
}

/// Side-by-side comparison of two runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunComparison {
    pub left: RunTimeline,
    pub right: RunTimeline,
    pub deltas: ComparisonDeltas,
}

/// Reconstruct the inspector timeline for `run_id`, scoped to `actor_id`.
///
/// Both the run row and its events are read through the store's public,
/// actor-validating methods, so a run owned by another actor fails closed with
/// [`StoreError::NotFound`]. A run with no events yields an empty timeline
/// (`total_duration_ms: None`), which is the Rust analogue of Python's
/// `build_run_timeline` returning `None`.
pub fn run_timeline(
    store: &LegacyStore,
    actor_id: &str,
    run_id: i64,
) -> Result<RunTimeline, StoreError> {
    let run = store.get_run(actor_id, run_id)?;
    let events = store.list_run_events(actor_id, run_id, None, None)?;
    Ok(build_timeline(&run, &events))
}

/// Build a timeline from an already-loaded run and its seq-ordered events.
fn build_timeline(run: &AgentRun, events: &[RunEvent]) -> RunTimeline {
    let entries = events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let payload = event.payload.as_ref();
            TimelineEntry {
                index,
                kind: event.kind.clone(),
                phase: phase_for(&event.kind).to_string(),
                status: status_for(&event.kind, payload),
                title: title_for(&event.kind, payload),
                occurred_at: event.created_at.clone(),
                duration_ms: duration_for(&event.kind, payload),
                payload: event.payload.clone(),
            }
        })
        .collect();

    RunTimeline {
        run_id: run.id,
        conversation_id: run.conversation_id,
        status: run.status.clone(),
        started_at: Some(run.started_at.clone()),
        finished_at: run.finished_at.clone(),
        total_duration_ms: total_duration_ms(events),
        entries,
        usage: run.usage.clone(),
        error: run.error.clone(),
    }
}

/// Numeric delta `right - left`, or `None` when either side is absent.
fn delta_i64(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(right - left),
        _ => None,
    }
}

/// Compare two runs and their timelines, scoped to `actor_id`.
pub fn compare_runs(
    store: &LegacyStore,
    actor_id: &str,
    left_run_id: i64,
    right_run_id: i64,
) -> Result<RunComparison, StoreError> {
    let left_run = store.get_run(actor_id, left_run_id)?;
    let right_run = store.get_run(actor_id, right_run_id)?;
    let left = run_timeline(store, actor_id, left_run_id)?;
    let right = run_timeline(store, actor_id, right_run_id)?;

    let deltas = ComparisonDeltas {
        // Python compares wall-clock durations, not the event-derived totals.
        duration_ms: delta_i64(run_wall_clock_ms(&left_run), run_wall_clock_ms(&right_run)),
        event_count: count_delta(left.entries.len(), right.entries.len()),
        total_tokens: Some(
            usage_total_tokens(right.usage.as_ref()) - usage_total_tokens(left.usage.as_ref()),
        ),
        cost_usd: match (
            usage_cost(left.usage.as_ref()),
            usage_cost(right.usage.as_ref()),
        ) {
            (Some(left_cost), Some(right_cost)) => Some(right_cost - left_cost),
            _ => None,
        },
        tool_calls: count_kind(&right.entries, "tool_call_start")
            - count_kind(&left.entries, "tool_call_start"),
    };

    Ok(RunComparison {
        left,
        right,
        deltas,
    })
}

/// Coarse phase grouping for an event kind. Unknown kinds fall back to `other`.
fn phase_for(kind: &str) -> &'static str {
    match kind {
        "start" => "start",
        "llm_call_complete" => "llm",
        "message" => "message",
        "thinking" | "react_thought" | "react_action" | "react_observation" => "reasoning",
        "tool_call_start"
        | "tool_call_delta"
        | "tool_result"
        | "tool_approval_request"
        | "tool_approval_resolved" => "tools",
        "plan_generated" | "plan_step_start" | "plan_step_complete" | "plan_progress" => "planning",
        "subagent_started" | "subagent_progress" | "subagent_completed" | "subagent_failed" => {
            "subagents"
        }
        "budget_alert" => "budget",
        "finish" | "error" => "finalize",
        _ => "other",
    }
}

/// Duration carried by an event payload, if any.
fn duration_for(kind: &str, payload: Option<&Value>) -> Option<i64> {
    match kind {
        "llm_call_complete" => payload
            .and_then(|payload| payload.get("duration_ms"))
            .and_then(Value::as_i64),
        "tool_result" => payload
            .and_then(|payload| payload.get("result"))
            .and_then(|result| result.get("metadata"))
            .and_then(|metadata| metadata.get("duration_ms"))
            .and_then(Value::as_i64),
        "finish" => payload
            .and_then(|payload| payload.get("elapsed_ms"))
            .and_then(Value::as_i64),
        _ => None,
    }
}

/// Derived status for an event, using the same mapping as the Python runner.
fn status_for(kind: &str, payload: Option<&Value>) -> Option<String> {
    match kind {
        "finish" => Some(finish_reason_to_status(payload_reason(payload)).to_string()),
        "error" => Some("failed".to_string()),
        "tool_result" => {
            let is_error = payload
                .and_then(|payload| payload.get("result"))
                .and_then(|result| result.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(if is_error { "failed" } else { "completed" }.to_string())
        }
        "tool_approval_resolved" => payload
            .and_then(|payload| payload.get("decision"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Short human label for an event, when a natural one exists.
fn title_for(kind: &str, payload: Option<&Value>) -> Option<String> {
    let field = match kind {
        "llm_call_complete" => "model",
        "tool_call_start" | "tool_call_delta" | "tool_result" | "tool_approval_request" => "name",
        "finish" => "reason",
        "error" => "message",
        _ => return None,
    };
    payload
        .and_then(|payload| payload.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Translate an executor finish reason into a run status (Python parity).
fn finish_reason_to_status(reason: Option<&str>) -> &'static str {
    match reason {
        Some(
            "stop" | "end_turn" | "tool_limit" | "token_limit" | "cost_limit" | "budget_exceeded"
            | "max_iterations",
        ) => "completed",
        Some("cancelled") => "cancelled",
        Some("error") => "failed",
        _ => "completed",
    }
}

fn payload_reason(payload: Option<&Value>) -> Option<&str> {
    payload
        .and_then(|payload| payload.get("reason"))
        .and_then(Value::as_str)
}

/// Event-derived total, mirroring `build_run_timeline` exactly.
fn total_duration_ms(events: &[RunEvent]) -> Option<i64> {
    let mut total = 0_i64;
    for event in events {
        match event.kind.as_str() {
            "llm_call_complete" => {
                if let Some(duration) = event
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("duration_ms"))
                    .and_then(Value::as_i64)
                {
                    total += duration;
                }
            }
            "finish" if total == 0 => {
                if let Some(elapsed) = event
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("elapsed_ms"))
                    .and_then(Value::as_i64)
                {
                    total = elapsed;
                }
            }
            _ => {}
        }
    }
    (total != 0).then_some(total)
}

/// Wall-clock run duration in milliseconds (Python `_run_duration_ms`).
fn run_wall_clock_ms(run: &AgentRun) -> Option<i64> {
    let started = parse_python_datetime(&run.started_at)?;
    let finished = parse_python_datetime(run.finished_at.as_deref()?)?;
    Some((finished - started) * 1_000)
}

fn usage_total_tokens(usage: Option<&Value>) -> i64 {
    usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn usage_cost(usage: Option<&Value>) -> Option<f64> {
    usage
        .and_then(|usage| usage.get("cost_usd"))
        .and_then(Value::as_f64)
}

fn count_kind(entries: &[TimelineEntry], kind: &str) -> i64 {
    i64::try_from(entries.iter().filter(|entry| entry.kind == kind).count()).unwrap_or(i64::MAX)
}

fn count_delta(left: usize, right: usize) -> i64 {
    i64::try_from(right).unwrap_or(i64::MAX) - i64::try_from(left).unwrap_or(i64::MAX)
}
