//! Write-through projection of canonical session-run events into the legacy
//! `agent_runs`/`run_events`/`tool_calls`/`spend_log` read models the React
//! Inspector, Analytics and Budgets pages consume (B2/B7).
//!
//! The durable event log is the source of truth; nothing else feeds these
//! tables. Every event appended on a run whose session is bound to a legacy
//! conversation lands here, so `runs.list`, `inspector.timeline`/`compare` and
//! the analytics aggregations see the session runs the chat pipeline actually
//! executed. Projection is best-effort: a mirror failure never rejects the
//! canonical write that produced it — it logs a warning and leaves the row's
//! `config.mirroredSeq` watermark behind, which the startup reconciliation
//! sweep (`reconcile_legacy_mirror`) resumes above.
//!
//! # Vocabulary mapping
//!
//! The mirrored `run_events` use the Python inspector vocabulary from
//! `crates/cool-store/tests/fixtures/timeline_parity.json` (`start`,
//! `message`, `llm_call_complete`, `tool_call_start`, `tool_result`,
//! `tool_approval_request`, `tool_approval_resolved`, `error`, `finish`) so
//! the Inspector grouping semantics hold; canonical kinds with no legacy
//! analogue pass through under their `kind` name (`item.started`,
//! `tool.started`, `plan.*`, `subagent.*`, ...). `usage.updated` is
//! per-model-call, so each one records an `llm_call_complete` entry, an
//! `iterations` bump and a `spend_log` row.
//!
//! # `rewound` copies
//!
//! `session.rewind` clones the retained history prefix into the seed run with
//! `extensions.rewound = true`. Those copies still project their timeline
//! `run_events` rows (the canonical log contains them again) but skip every
//! accounting write — `spend_log`, `tool_calls`, `usage`/`iterations` and
//! terminal `finish_run` — since the superseded run's rows already recorded
//! them once.

use cool_protocol::{CanonicalEvent, EventEnvelope};
use cool_state::{DurableStore, RunStatus, SessionRewindOutcome, SessionRunEntry};
use cool_store::LegacyStore;
use cool_store::domains::budgets::NewSpendEntry;
use cool_store::domains::runs::{AgentRun, NewRun, NewToolCall, RunProgress};
use cool_store::time::{now_python, parse_python_datetime, python_datetime};
use serde_json::{Value, json};
use tracing::warn;

use crate::{AppServer, local_actor};

impl AppServer {
    /// Mirror one appended durable event into the legacy read models when the
    /// run's session is bound to a conversation. Called from every append
    /// path (the sink funnel, cancel acceptance, steer, rewind, recovery).
    pub(crate) fn mirror_to_legacy(&self, envelope: &EventEnvelope) {
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return;
        };
        if let Err(error) = project(legacy, &self.inner.store, &local_actor().id, envelope) {
            warn!(
                run_id = %envelope.run_id,
                seq = envelope.seq,
                "legacy mirror projection failed: {error}"
            );
        }
    }

    /// Mirror the envelopes `recover_incomplete_runs` emitted while marking
    /// crashed runs terminal — the rows they touch would otherwise stay
    /// `running` forever (zombie picker entries).
    pub(crate) fn mirror_recovered(&self, recovered: &[EventEnvelope]) {
        for envelope in recovered {
            self.mirror_to_legacy(envelope);
        }
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
        match self
            .inner
            .store
            .events(&outcome.run_id, &actor.id, None, usize::MAX)
        {
            Ok(events) => {
                for envelope in &events {
                    self.mirror_to_legacy(envelope);
                }
            }
            Err(error) => warn!(
                run_id = %outcome.run_id,
                "legacy mirror could not read rewind seed events: {error}"
            ),
        }
        for durable_run_id in &outcome.rewound_run_ids {
            if let Err(error) = close_terminal_run(
                legacy,
                &self.inner.store,
                &actor.id,
                durable_run_id,
                "cancelled",
                Some("rewound"),
                None,
            ) {
                warn!(
                    run_id = %durable_run_id,
                    "legacy mirror could not close rewound run: {error}"
                );
            }
        }
    }

    /// Startup reconciliation sweep. Enumerates every conversation-linked
    /// session's canonical runs and replays the events past each mirrored
    /// row's `config.mirroredSeq` watermark — backfilling runs that predate
    /// the mirror (or missed a write mid-crash) — then closes rows still
    /// open whose canonical run is already terminal. `import` runs (the
    /// link-time transcript projection), compaction and subagent-lifecycle
    /// pseudo-runs are not picker material and stay unmirrored.
    pub(crate) fn reconcile_legacy_mirror(&self) {
        if self.inner.config.legacy_store.is_none() {
            return;
        }
        let actor = local_actor();
        let links = match self.inner.store.linked_sessions(&actor.id) {
            Ok(links) => links,
            Err(error) => {
                warn!("legacy mirror sweep could not list linked sessions: {error}");
                return;
            }
        };
        for (session_id, conversation_id) in links {
            self.reconcile_linked_session(&actor.id, conversation_id, &session_id);
        }
    }

    /// Per-conversation half of `reconcile_legacy_mirror`, also invoked when
    /// a conversation binds its durable session after startup: pre-link
    /// runs of that session mirror now rather than on the next restart.
    /// Replays at most `MIRROR_SWEEP_EVENTS_PER_RUN` events per run — the
    /// watermark makes the sweep resumable, so a deeper backlog finishes on
    /// the next restart instead of blocking this boot (or link).
    pub(crate) fn reconcile_linked_session(
        &self,
        actor_id: &str,
        conversation_id: i64,
        session_id: &str,
    ) {
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return;
        };
        let runs = match self
            .inner
            .store
            .list_session_runs(actor_id, session_id, usize::MAX)
        {
            Ok(runs) => runs,
            Err(error) => {
                warn!(
                    session_id = %session_id,
                    "legacy mirror sweep could not list runs: {error}"
                );
                return;
            }
        };
        for run in runs {
            if is_auxiliary_run(&run) {
                continue;
            }
            let cursor = legacy
                .find_run_by_durable_id(actor_id, conversation_id, &run.run_id)
                .ok()
                .flatten()
                .and_then(|row| mirror_cursor(&row));
            match self.inner.store.events(
                &run.run_id,
                actor_id,
                cursor,
                MIRROR_SWEEP_EVENTS_PER_RUN,
            ) {
                Ok(events) => {
                    for envelope in &events {
                        self.mirror_to_legacy(envelope);
                    }
                }
                Err(error) => warn!(
                    run_id = %run.run_id,
                    "legacy mirror sweep could not read events: {error}"
                ),
            }
            // A canonical run already terminal whose mirrored row never
            // closed (crash between the writes, or pre-mirror drift).
            if run.status.is_terminal()
                && let Err(error) = close_terminal_run(
                    legacy,
                    &self.inner.store,
                    actor_id,
                    &run.run_id,
                    legacy_status(run.status),
                    run.finish_reason.as_deref(),
                    None,
                )
            {
                warn!(
                    run_id = %run.run_id,
                    "legacy mirror sweep could not close run: {error}"
                );
            }
        }
    }
}

/// Per-run replay budget for a single reconciliation pass; the
/// `mirroredSeq` watermark makes the sweep resumable, so capping each run
/// bounds first-boot (and link-time) latency on large stores — a deeper
/// backlog resumes where it stopped on the next pass rather than needing
/// an unbounded replay here.
const MIRROR_SWEEP_EVENTS_PER_RUN: usize = 4096;

/// Runs that exist only to host bookkeeping events are not picker or
/// Analytics material: compaction projections and subagent lifecycle rows
/// have their own domains, and the link-time `import` run is a transcript
/// projection, not an execution. `purpose` covers runs still in flight;
/// `finish_reason` covers rows written before the purpose column existed.
/// User-facing aux runs (`replay_exec`, `research_exec`) and crash-recovered
/// real runs (`core_restarted`) keep mirroring.
fn is_auxiliary_run(run: &SessionRunEntry) -> bool {
    matches!(run.purpose.as_deref(), Some("compact") | Some("subagent"))
        || matches!(
            run.finish_reason.as_deref(),
            Some("import") | Some("compact")
        )
        || run
            .finish_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("subagent_"))
}

/// Anything the projection can hit — the mirror is best-effort and swallows
/// at the top, so one box covers both store error types.
type MirrorResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Project `envelope` into the legacy read models. A no-op when the run's
/// session is not conversation-bound — the React surfaces only ever read
/// conversations. Events already covered by the row's `mirroredSeq`
/// watermark skip outright, making projection single-shot.
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
    if mirror_cursor(&row).is_some_and(|cursor| envelope.seq <= cursor) {
        return Ok(());
    }
    // Rewind-copied events emit only their timeline row — the superseded
    // run's rows already recorded the accounting side.
    let accounting = !envelope
        .extensions
        .get("rewound")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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
            if accounting {
                row = legacy.update_run_progress(
                    actor_id,
                    row.id,
                    &RunProgress {
                        status: Some("running".to_owned()),
                        model: started.model.clone(),
                        ..RunProgress::default()
                    },
                )?;
            }
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
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "tool_approval_request",
                Some(&json!({
                    "id": ask.call_id,
                    "name": ask.name,
                    "arguments": ask.arguments,
                    "reason": ask.reason,
                    "requires_decision": true,
                    "approval_id": ask.approval_id,
                    "revision": ask.revision,
                    "run_id": row.id,
                    "is_breakpoint": ask.breakpoint_type.is_some(),
                    "breakpoint_type": ask.breakpoint_type,
                    "result_preview": ask.result_preview,
                    "current_content": ask.current_content,
                })),
                &at,
            )?;
            if accounting {
                row = legacy.update_run_progress(
                    actor_id,
                    row.id,
                    &RunProgress {
                        status: Some("awaiting_approval".to_owned()),
                        ..RunProgress::default()
                    },
                )?;
            }
        }
        CanonicalEvent::ToolApprovalResolved(resolution) => {
            legacy.append_run_event_at(
                actor_id,
                row.id,
                "tool_approval_resolved",
                Some(&json!({
                    "id": resolution.call_id,
                    "approval_id": resolution.approval_id,
                    "revision": resolution.revision,
                    "decision": resolution.decision,
                })),
                &at,
            )?;
            if accounting {
                row = legacy.update_run_progress(
                    actor_id,
                    row.id,
                    &RunProgress {
                        status: Some("running".to_owned()),
                        ..RunProgress::default()
                    },
                )?;
            }
        }
        CanonicalEvent::ToolCompleted(completed) => {
            record_tool_result(
                legacy,
                actor_id,
                conversation_id,
                &row,
                accounting,
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
                accounting,
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
            if accounting {
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
            finish(
                legacy,
                actor_id,
                &row,
                "completed",
                &terminal.reason,
                None,
                accounting,
                &at,
            )?;
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
                &terminal.reason,
                Some(&terminal.reason),
                accounting,
                &at,
            )?;
        }
        CanonicalEvent::RunCancelled(terminal) => {
            finish(
                legacy,
                actor_id,
                &row,
                "cancelled",
                &terminal.reason,
                None,
                accounting,
                &at,
            )?;
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
    legacy.set_run_mirror_cursor(actor_id, row.id, envelope.seq)?;
    Ok(())
}

/// Find-or-create the legacy run row bound to `envelope.run_id` — atomic in
/// the store layer so racing first-events can't duplicate the row.
fn ensure_run(
    legacy: &LegacyStore,
    actor_id: &str,
    conversation_id: i64,
    envelope: &EventEnvelope,
) -> MirrorResult<AgentRun> {
    Ok(legacy.ensure_run_by_durable_id(
        actor_id,
        conversation_id,
        &envelope.run_id,
        &NewRun {
            config: Some(json!({
                "source": "session-run",
            })),
            ..NewRun::default()
        },
    )?)
}

/// The canonical `seq` the row already mirrored, from `config.mirroredSeq`.
fn mirror_cursor(row: &AgentRun) -> Option<u64> {
    row.config
        .as_ref()
        .and_then(|config| config.get("mirroredSeq"))
        .and_then(Value::as_u64)
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

/// `tool_result` run entry plus — unless this is a rewound copy — the
/// `tool_calls` analytics row. The call's arguments and start time come back
/// from the mirrored `tool_call_start`.
#[allow(clippy::too_many_arguments)]
fn record_tool_result(
    legacy: &LegacyStore,
    actor_id: &str,
    conversation_id: i64,
    row: &AgentRun,
    accounting: bool,
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
    if !accounting {
        return Ok(());
    }
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

/// Append the `finish` entry and — unless this is a rewound copy — close the
/// legacy row (`elapsed_ms` is the wall-clock gap since the row's
/// `started_at`, matching the Python runner).
#[allow(clippy::too_many_arguments)]
fn finish(
    legacy: &LegacyStore,
    actor_id: &str,
    row: &AgentRun,
    status: &str,
    reason: &str,
    error: Option<&str>,
    accounting: bool,
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
            "reason": reason,
            "usage": row.usage,
        })),
        at,
    )?;
    if !accounting {
        return Ok(());
    }
    legacy.finish_run(
        actor_id,
        row.id,
        status,
        row.usage.as_ref(),
        Some(row.iterations),
        Some(reason),
        error,
    )?;
    Ok(())
}

/// The legacy status word for a terminal canonical `RunStatus` (`rewound`
/// has no legacy equivalent — the row ends `cancelled` with the reason
/// preserved as `finish_reason`).
fn legacy_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled | RunStatus::Rewound => "cancelled",
        RunStatus::Queued | RunStatus::Running | RunStatus::AwaitingApproval => "running",
    }
}

/// Close the mirrored row of a canonical run that already finished — used by
/// the rewind path (`cancelled`/`rewound`) and the startup sweep (whichever
/// terminal status the canonical run carries). No-op when the row never
/// existed or already finished.
#[allow(clippy::too_many_arguments)]
fn close_terminal_run(
    legacy: &LegacyStore,
    durable: &DurableStore,
    actor_id: &str,
    durable_run_id: &str,
    status: &str,
    finish_reason: Option<&str>,
    error: Option<&str>,
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
            "reason": finish_reason,
            "usage": row.usage,
        })),
        &now_python(),
    )?;
    legacy.finish_run(
        actor_id,
        row.id,
        status,
        row.usage.as_ref(),
        Some(row.iterations),
        finish_reason,
        error,
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
