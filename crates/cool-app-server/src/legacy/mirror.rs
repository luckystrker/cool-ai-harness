//! Write-through projection of canonical session-run events into the legacy
//! `agent_runs`/`run_events`/`tool_calls`/`spend_log` read models the React
//! Inspector, Analytics and Budgets pages consume (B2/B7).
//!
//! The durable event log is the source of truth; nothing else feeds these
//! tables. Every event appended on a run whose session is bound to a legacy
//! conversation lands here, so `runs.list`, `inspector.timeline`/`compare` and
//! the analytics aggregations see the session runs the chat pipeline actually
//! executed. Projection is best-effort: a mirror failure never rejects the
//! canonical write that produced it.
//!
//! # Vocabulary mapping
//!
//! The mirrored `run_events` use the Python inspector vocabulary from
//! `crates/cool-store/tests/fixtures/timeline_parity.json` (`start`,
//! `message`, `llm_call_complete`, `tool_call_start`, `tool_result`, `error`,
//! `finish`) so the Inspector grouping semantics hold; canonical kinds with no
//! Python analogue pass through under their `kind` name. `usage.updated` is
//! per-model-call, so each one records an `llm_call_complete` entry, an
//! `iterations` bump and a `spend_log` row.

use cool_protocol::{CanonicalEvent, EventEnvelope};
use cool_state::{DurableStore, SessionRewindOutcome};
use cool_store::LegacyStore;
use cool_store::domains::budgets::NewSpendEntry;
use cool_store::domains::runs::{AgentRun, NewRun, NewToolCall, RunProgress};
use cool_store::time::{now_python, parse_python_datetime, python_datetime};
use serde_json::{Value, json};

use crate::{AppServer, local_actor};

impl AppServer {
    /// Mirror one appended durable event into the legacy read models when the
    /// run's session is bound to a conversation. Called from every append
    /// path (the sink funnel, cancel acceptance, steer, compaction).
    pub(crate) fn mirror_to_legacy(&self, envelope: &EventEnvelope) {
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return;
        };
        let _ = project(legacy, &self.inner.store, &local_actor().id, envelope);
    }

    /// Reconcile the mirror after `session.rewind`: close the mirrored rows of
    /// the superseded runs and project the seed run's events — the rewind
    /// writes them inside one store transaction, so the per-event hook in
    /// `append_event` never sees them.
    pub(crate) fn mirror_rewind(&self, outcome: &SessionRewindOutcome) {
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return;
        };
        let actor = local_actor();
        if let Ok(events) = self
            .inner
            .store
            .events(&outcome.run_id, &actor.id, None, usize::MAX)
        {
            for envelope in &events {
                self.mirror_to_legacy(envelope);
            }
        }
        for durable_run_id in &outcome.rewound_run_ids {
            let _ = close_rewound_run(legacy, &self.inner.store, &actor.id, durable_run_id);
        }
    }
}

/// Anything the projection can hit — the mirror is best-effort and swallows
/// at the top, so one box covers both store error types.
type MirrorResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Project `envelope` into the legacy read models. A no-op when the run's
/// session is not conversation-bound — the React surfaces only ever read
/// conversations.
fn project(
    legacy: &LegacyStore,
    durable: &DurableStore,
    actor_id: &str,
    envelope: &EventEnvelope,
) -> MirrorResult<()> {
    let run = durable.run(&envelope.run_id, actor_id)?;
    let Some(conversation_id) = durable.conversation_id_for_session(actor_id, &run.session_id)?
    else {
        return Ok(());
    };
    let at = occurred_at_python(&envelope.occurred_at);
    let mut row = ensure_run(legacy, actor_id, conversation_id, envelope)?;
    match &envelope.event {
        // Streaming noise the legacy vocabulary never carried.
        CanonicalEvent::ContentDelta(_) | CanonicalEvent::ItemUpdated(_) => {}
        CanonicalEvent::RunStarted(started) => {
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "start",
                Some(&json!({
                    "run_id": null,
                    "model": started.model,
                    "mode": started.mode,
                })),
                &at,
            )?;
            legacy.update_run_progress(
                actor_id,
                row.id,
                &RunProgress {
                    status: Some("running".to_owned()),
                    model: started.model.clone(),
                    ..RunProgress::default()
                },
            )?;
        }
        CanonicalEvent::ItemCompleted(item) => {
            let role = item.role.as_deref().unwrap_or("assistant");
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "message",
                Some(&json!({
                    "role": role,
                    "content": item.content,
                    "thinking": null,
                    "tool_calls": item
                        .tool_calls
                        .iter()
                        .map(|call| json!({"id": call.call_id, "name": call.name}))
                        .collect::<Vec<_>>(),
                })),
                &at,
            )?;
        }
        CanonicalEvent::ReasoningDelta(delta) => {
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "thinking",
                Some(&json!({"content": delta.text})),
                &at,
            )?;
        }
        CanonicalEvent::ItemStarted(item) => {
            record(
                legacy,
                actor_id,
                &row,
                "item.started",
                serde_json::to_value(item)?,
                &at,
            )?;
        }
        CanonicalEvent::ToolRequested(call) => {
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "tool_call_start",
                Some(&json!({
                    "id": call.call_id,
                    "name": call.name,
                    "arguments": call.arguments,
                })),
                &at,
            )?;
        }
        CanonicalEvent::ToolStarted(lifecycle) => {
            record(
                legacy,
                actor_id,
                &row,
                "tool.started",
                serde_json::to_value(lifecycle)?,
                &at,
            )?;
        }
        CanonicalEvent::ToolApprovalRequired(ask) => {
            record(
                legacy,
                actor_id,
                &row,
                "tool.approval_required",
                serde_json::to_value(ask)?,
                &at,
            )?;
            legacy.update_run_progress(
                actor_id,
                row.id,
                &RunProgress {
                    status: Some("awaiting_approval".to_owned()),
                    ..RunProgress::default()
                },
            )?;
        }
        CanonicalEvent::ToolApprovalResolved(resolution) => {
            record(
                legacy,
                actor_id,
                &row,
                "tool.approval_resolved",
                serde_json::to_value(resolution)?,
                &at,
            )?;
            legacy.update_run_progress(
                actor_id,
                row.id,
                &RunProgress {
                    status: Some("running".to_owned()),
                    ..RunProgress::default()
                },
            )?;
        }
        CanonicalEvent::ToolCompleted(completed) => {
            record_tool_result(
                legacy,
                actor_id,
                conversation_id,
                &row,
                &completed.call_id,
                &completed.name,
                false,
                completed.result.clone(),
                None,
                &at,
            )?;
        }
        CanonicalEvent::ToolFailed(failed) => {
            record_tool_result(
                legacy,
                actor_id,
                conversation_id,
                &row,
                &failed.call_id,
                &failed.name,
                true,
                failed
                    .message
                    .clone()
                    .map_or_else(|| json!({"error": failed.error_code}), Value::String),
                Some(failed.error_code.clone()),
                &at,
            )?;
        }
        CanonicalEvent::UsageUpdated(usage) => {
            // Canonical usage is per-call: the gap since the previous recorded
            // activity approximates the call's wall-clock cost (the envelope
            // carries no duration field).
            let duration_ms = legacy
                .last_run_event_created_at(actor_id, row.id)?
                .map(|previous| {
                    (parse_python_datetime(&at).unwrap_or_default()
                        - parse_python_datetime(&previous).unwrap_or_default())
                    .max(0)
                        * 1_000
                });
            let delta = json!({
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
                "cost_usd": usage.cost_usd.unwrap_or(0.0),
            });
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "llm_call_complete",
                Some(&json!({
                    "duration_ms": duration_ms,
                    "iteration": row.iterations + 1,
                    "model": row.model,
                    "usage": delta,
                })),
                &at,
            )?;
            row = legacy.update_run_progress(
                actor_id,
                row.id,
                &RunProgress {
                    usage_delta: Some(delta),
                    iterations_delta: 1,
                    ..RunProgress::default()
                },
            )?;
            legacy.log_spend(
                actor_id,
                &NewSpendEntry {
                    run_id: Some(row.id),
                    conversation_id: Some(conversation_id),
                    provider_name: provider_name(legacy, actor_id, row.model.as_deref()),
                    model: row.model.clone().unwrap_or_else(|| "unknown".to_owned()),
                    prompt_tokens: usage.prompt_tokens as i64,
                    completion_tokens: usage.completion_tokens as i64,
                    total_tokens: usage.total_tokens as i64,
                    cost_usd: usage.cost_usd.unwrap_or(0.0),
                    ts: Some(at.clone()),
                },
            )?;
        }
        CanonicalEvent::BudgetWarning(budget) | CanonicalEvent::BudgetExceeded(budget) => {
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "budget_alert",
                Some(&serde_json::to_value(budget)?),
                &at,
            )?;
        }
        CanonicalEvent::RunCompleted(terminal) => {
            finish(legacy, actor_id, &row, "completed", terminal, None, &at)?;
        }
        CanonicalEvent::RunFailed(terminal) => {
            let detail = terminal.error_code.as_deref().unwrap_or(&terminal.reason);
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "error",
                Some(&json!({"message": terminal.reason, "detail": detail})),
                &at,
            )?;
            finish(
                legacy,
                actor_id,
                &row,
                "failed",
                terminal,
                Some(&terminal.reason),
                &at,
            )?;
        }
        CanonicalEvent::RunCancelled(terminal) => {
            finish(legacy, actor_id, &row, "cancelled", terminal, None, &at)?;
        }
        other => {
            // Faithful passthrough under the canonical kind name (`plan.*`,
            // `subagent.*`, `session.*`, `research.*`, ...) so the Inspector
            // event feed keeps the full trace.
            let value = serde_json::to_value(other)?;
            let kind = value
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let payload = value.get("payload").cloned();
            legacy.append_run_event_at(actor_id, row.id, kind, payload.as_ref(), &at)?;
        }
    }
    Ok(())
}

/// Find-or-create the legacy run row bound to `envelope.run_id`.
fn ensure_run(
    legacy: &LegacyStore,
    actor_id: &str,
    conversation_id: i64,
    envelope: &EventEnvelope,
) -> MirrorResult<AgentRun> {
    if let Some(row) = legacy.find_run_by_durable_id(actor_id, conversation_id, &envelope.run_id)? {
        return Ok(row);
    }
    Ok(legacy.create_run(
        actor_id,
        conversation_id,
        &NewRun {
            config: Some(json!({
                "durableRunId": envelope.run_id,
                "source": "session-run",
            })),
            ..NewRun::default()
        },
    )?)
}

/// Append one entry under the run — shorthand for the passthrough arms.
fn record(
    legacy: &LegacyStore,
    actor_id: &str,
    row: &AgentRun,
    kind: &str,
    payload: Value,
    at: &str,
) -> MirrorResult<()> {
    legacy.append_run_event_at(actor_id, row.id, kind, Some(&payload), at)?;
    Ok(())
}

/// `tool_result` run entry plus the `tool_calls` analytics row. The call's
/// arguments and start time come back from the mirrored `tool_call_start`.
#[allow(clippy::too_many_arguments)]
fn record_tool_result(
    legacy: &LegacyStore,
    actor_id: &str,
    conversation_id: i64,
    row: &AgentRun,
    call_id: &str,
    name: &str,
    is_error: bool,
    output: Value,
    error: Option<String>,
    at: &str,
) -> MirrorResult<()> {
    let started = legacy.tool_call_started(actor_id, row.id, call_id)?;
    let duration_ms = started.as_ref().and_then(|event| {
        Some(
            (parse_python_datetime(at)? - parse_python_datetime(&event.created_at)?).max(0) * 1_000,
        )
    });
    let arguments = started
        .and_then(|event| event.payload)
        .and_then(|payload| payload.get("arguments").cloned());
    legacy.append_run_event_at(
        actor_id,
        row.id,
        "tool_result",
        Some(&json!({
            "id": call_id,
            "name": name,
            "result": {
                "is_error": is_error,
                "metadata": {"duration_ms": duration_ms},
                "output": output,
            },
        })),
        at,
    )?;
    legacy.record_tool_call(
        actor_id,
        &NewToolCall {
            conversation_id: Some(conversation_id),
            message_id: None,
            name: name.to_owned(),
            arguments,
            result: Some(output),
            duration_ms,
            success: !is_error,
            error,
        },
    )?;
    Ok(())
}

/// Append the `finish` entry and close the legacy row (`elapsed_ms` is the
/// wall-clock gap since the row's `started_at`, matching the Python runner).
fn finish(
    legacy: &LegacyStore,
    actor_id: &str,
    row: &AgentRun,
    status: &str,
    terminal: &cool_protocol::RunTerminal,
    error: Option<&str>,
    at: &str,
) -> MirrorResult<()> {
    let elapsed_ms = parse_python_datetime(at)
        .zip(parse_python_datetime(&row.started_at))
        .map(|(finished, started)| (finished - started).max(0) * 1_000);
    legacy.append_run_event_at(
        actor_id,
        row.id,
        "finish",
        Some(&json!({
            "elapsed_ms": elapsed_ms,
            "iterations": row.iterations,
            "reason": terminal.reason,
            "usage": row.usage,
        })),
        at,
    )?;
    legacy.finish_run(
        actor_id,
        row.id,
        status,
        row.usage.as_ref(),
        Some(row.iterations),
        Some(&terminal.reason),
        error,
    )?;
    Ok(())
}

/// Close the mirrored row of a run `session.rewind` superseded. Durable
/// `rewound` has no legacy status equivalent, so the row ends `cancelled`
/// with `finish_reason = "rewound"`.
fn close_rewound_run(
    legacy: &LegacyStore,
    durable: &DurableStore,
    actor_id: &str,
    durable_run_id: &str,
) -> MirrorResult<()> {
    let run = durable.run(durable_run_id, actor_id)?;
    let Some(conversation_id) = durable.conversation_id_for_session(actor_id, &run.session_id)?
    else {
        return Ok(());
    };
    let Some(row) = legacy.find_run_by_durable_id(actor_id, conversation_id, durable_run_id)?
    else {
        return Ok(());
    };
    if row.finished_at.is_some() {
        return Ok(());
    }
    legacy.append_run_event_at(
        actor_id,
        row.id,
        "finish",
        Some(&json!({
            "elapsed_ms": null,
            "iterations": row.iterations,
            "reason": "rewound",
            "usage": row.usage,
        })),
        &now_python(),
    )?;
    legacy.finish_run(
        actor_id,
        row.id,
        "cancelled",
        row.usage.as_ref(),
        Some(row.iterations),
        Some("rewound"),
        None,
    )?;
    Ok(())
}

/// The `provider_name` a spend row reports: the active provider that owns the
/// run's model, else the first active provider, else `unknown` — the durable
/// model drivers carry no provider identity.
fn provider_name(legacy: &LegacyStore, actor_id: &str, model: Option<&str>) -> String {
    let providers = legacy.list_providers(actor_id, false).unwrap_or_default();
    providers
        .iter()
        .find(|provider| provider.default_model.as_deref() == model && model.is_some())
        .or_else(|| providers.first())
        .map_or_else(|| "unknown".to_owned(), |provider| provider.name.clone())
}

/// Canonical `occurred_at` (RFC 3339) → the legacy `YYYY-MM-DD HH:MM:SS`
/// timestamp format. Falls back to wall-clock on a malformed value.
fn occurred_at_python(value: &str) -> String {
    let Some(seconds) = parse_python_datetime(value) else {
        return now_python();
    };
    let micros = value
        .split_once('.')
        .map(|(_, fraction)| {
            let digits: String = fraction
                .chars()
                .take_while(char::is_ascii_digit)
                .take(6)
                .collect();
            format!("{:0<6}", digits).parse().unwrap_or(0)
        })
        .unwrap_or(0);
    python_datetime(seconds, micros)
}
