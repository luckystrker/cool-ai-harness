//! Background task scheduler/executor (M11).
//!
//! Ties the deterministic `cool_store::scheduler` engine to the Rust agent
//! runtime: due tasks are planned, queued as durable `task_runs` rows, executed
//! through `AgentRuntime`, and finalized with their output/usage. The executor
//! is actor-scoped to the local single-user actor, matching the rest of the
//! local facade; the `server` profile must revisit this when it introduces
//! multiple actors.
//!
//! Background task runs record their result in the durable `task_runs` table
//! (`status`/`output`/`usage`). They do not yet project into the canonical
//! `rust_events` log — that integration is tracked as an M11 residual.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, EventSink, Message,
    MessageRole, RunOutcome, RuntimeError, ToolContext,
};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, CanonicalEvent, EventEnvelope, SchedulerJobRecord,
    SchedulerStatusRecord, V1Version,
};
use cool_security::{Capability, CapabilityPolicy, Decision, Workspace, mask_secrets};
use cool_store::LegacyStore;
use cool_store::domains::tasks::{
    APPROVAL_ALLOW_ALL, NewScheduledTask, NewTaskRun, ScheduledTask, TASK_RUN_CANCELLED,
    TASK_RUN_COMPLETED, TASK_RUN_FAILED, TASK_RUN_RUNNING, TaskRun,
};
use cool_store::scheduler::{self, Decision as ScheduleDecision, Scheduler, SchedulerConfig};
use serde_json::Value;
use tokio::sync::{Mutex, watch};

/// Default number of concurrent task runs advertised to the UI (matches the
/// Python scheduler's `max_concurrent_tasks`).
const MAX_CONCURRENT_TASKS: u32 = 3;

/// Wall-clock cap for a task run without an explicit `timeout_s` (matches the
/// Python `scheduler_task_timeout_s` default).
const DEFAULT_TASK_TIMEOUT_SECONDS: f64 = 900.0;

/// Drives due scheduled tasks and manual `tasks.run` requests through the agent
/// runtime.
pub struct TaskExecutor {
    store: Arc<LegacyStore>,
    runtime: AgentRuntime,
    workspace: Workspace,
    policy: CapabilityPolicy,
    default_model: String,
    config: SchedulerConfig,
    engine: Mutex<Scheduler>,
    /// Live cancel channels per `task_runs.id`.
    live: Mutex<HashMap<i64, watch::Sender<Option<String>>>>,
    running: AtomicBool,
}

impl TaskExecutor {
    pub fn new(
        store: Arc<LegacyStore>,
        runtime: AgentRuntime,
        workspace: Workspace,
        policy: CapabilityPolicy,
        default_model: String,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            store,
            runtime,
            workspace,
            policy,
            default_model,
            config,
            engine: Mutex::new(Scheduler::new(config)),
            live: Mutex::new(HashMap::new()),
            running: AtomicBool::new(true),
        }
    }

    /// Engine status for `tasks.scheduler`.
    pub fn status(
        &self,
        actor: &ActorRef,
    ) -> Result<SchedulerStatusRecord, cool_store::StoreError> {
        let tasks = self.store.list_tasks(&actor.id, false)?;
        let jobs = tasks
            .iter()
            .filter(|task| task.enabled)
            .map(|task| SchedulerJobRecord {
                id: task.id.to_string(),
                name: task.name.clone(),
                next_run_time: task.next_run_at.clone(),
            })
            .collect();
        Ok(SchedulerStatusRecord {
            enabled: true,
            running: self.running.load(Ordering::Relaxed),
            timezone: "UTC".to_owned(),
            max_concurrent_tasks: MAX_CONCURRENT_TASKS,
            jobs,
        })
    }

    /// Run one scheduling tick: plan due tasks, record skips, and spawn an
    /// execution for each due task. A failure to enqueue one task is reported
    /// but does not abort the rest of the tick.
    pub async fn tick(self: &Arc<Self>, now: i64) -> Result<(), cool_store::StoreError> {
        let due = self.store.list_due_tasks(now)?;
        // A task whose quiet-hours window uses a timezone the Rust core cannot
        // interpret would make `Scheduler::plan` fail and stall every other
        // task; drop such rows from the batch (best-effort skip) so the rest of
        // the schedule keeps running.
        let mut tasks = Vec::with_capacity(due.len());
        for task in due {
            if let Err(error) = scheduler::quiet_hours(&task, now) {
                let actor = crate::local_actor();
                let _ = self.store.record_skipped_run(
                    &actor.id,
                    task.id,
                    &format!("unsupported schedule: {error}"),
                );
                continue;
            }
            tasks.push(task);
        }
        let decisions = self.engine.lock().await.plan(&tasks, now)?;
        let mut first_error = None;
        for decision in decisions {
            match decision {
                ScheduleDecision::Skip { task_id, reason } => {
                    let actor = crate::local_actor();
                    if let Err(error) = self.store.record_skipped_run(&actor.id, task_id, &reason)
                        && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                }
                ScheduleDecision::Execute { task_id, .. } => {
                    match tasks.iter().find(|task| task.id == task_id).cloned() {
                        Some(task) => {
                            if let Err(error) = self.enqueue(task, "schedule").await
                                && first_error.is_none()
                            {
                                first_error = Some(error);
                            }
                        }
                        None => self.engine.lock().await.complete(task_id),
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Trigger a task immediately (manual run), returning the running row.
    /// Idempotent on `(actor, key)`: a replay returns the original row without
    /// executing the task again.
    pub async fn run_now(
        self: &Arc<Self>,
        actor: &ActorRef,
        task_id: i64,
        idempotency_key: &str,
    ) -> Result<TaskRun, cool_store::StoreError> {
        let fingerprint = format!("tasks.run:{task_id}");
        let result = self
            .store
            .run_idempotent_async(
                &actor.id,
                "tasks.run",
                idempotency_key,
                &fingerprint,
                || async {
                    let task = self.store.get_task(&actor.id, task_id)?;
                    self.enqueue(task, "manual").await
                },
            )
            .await?;
        Ok(result.value)
    }

    /// Fire an existing task immediately for an out-of-band trigger (webhook
    /// replay), returning the durable run row. Actor-scoped like `tasks.run`.
    /// Quiet hours are enforced (Python `schedule_task_execution` without
    /// `ignore_quiet_hours`), so a blocked task yields a durable skipped run.
    /// The local single-actor executor assumption is inherited from `enqueue`.
    pub async fn dispatch_task(
        self: &Arc<Self>,
        actor: &ActorRef,
        task_id: i64,
    ) -> Result<TaskRun, cool_store::StoreError> {
        let task = self.store.get_task(&actor.id, task_id)?;
        match scheduler::quiet_hours(&task, crate::legacy::now_seconds()) {
            Ok(true) => {
                let reason = format!(
                    "quiet hours {} - {}",
                    task.quiet_hours_start.as_deref().unwrap_or(""),
                    task.quiet_hours_end.as_deref().unwrap_or("")
                );
                self.store.record_skipped_run(&actor.id, task.id, &reason)
            }
            Ok(false) => self.enqueue(task, "manual").await,
            Err(error) => self.store.record_skipped_run(
                &actor.id,
                task.id,
                &format!("unsupported schedule: {error}"),
            ),
        }
    }

    /// Create a one-shot disabled task for an ad-hoc prompt and run it now
    /// (Python `_dispatch_adhoc`), returning the durable run row.
    pub async fn dispatch_adhoc(
        self: &Arc<Self>,
        actor: &ActorRef,
        name: String,
        prompt: String,
    ) -> Result<TaskRun, cool_store::StoreError> {
        let task = self.store.create_task(
            &actor.id,
            &NewScheduledTask {
                name,
                prompt,
                trigger_type: "date".to_owned(),
                run_at: Some(cool_store::python_datetime(crate::legacy::now_seconds(), 0)),
                // One-shot: never recurs.
                enabled: false,
                ..NewScheduledTask::default()
            },
        )?;
        self.enqueue(task, "manual").await
    }

    /// Cancel a task run: signal the live execution (if any) and flip the row.
    /// Idempotent on `(actor, key)` like the other mutations.
    pub async fn cancel(
        self: &Arc<Self>,
        actor: &ActorRef,
        run_id: i64,
        idempotency_key: &str,
    ) -> Result<TaskRun, cool_store::StoreError> {
        let fingerprint = format!("tasks.runs_cancel:{run_id}");
        let result = self
            .store
            .run_idempotent_async(
                &actor.id,
                "tasks.runs_cancel",
                idempotency_key,
                &fingerprint,
                || async {
                    if let Some(sender) = self.live.lock().await.remove(&run_id) {
                        let _ = sender.send(Some("cancelled".to_owned()));
                    }
                    self.store.cancel_task_run(&actor.id, run_id)
                },
            )
            .await?;
        Ok(result.value)
    }

    /// Start the periodic loop. The returned handle is detached by callers.
    pub fn spawn_loop(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let now = crate::legacy::now_seconds();
                let _ = self.tick(now).await;
                tokio::time::sleep(interval).await;
            }
        })
    }

    /// Mark a task in-flight, create its durable run row, and spawn execution.
    /// Any failure clears the in-flight mark so the task can fire again.
    async fn enqueue(
        self: &Arc<Self>,
        task: ScheduledTask,
        trigger: &str,
    ) -> Result<TaskRun, cool_store::StoreError> {
        let task_id = task.id;
        self.engine.lock().await.mark_running(task_id);
        match self.enqueue_inner(task, trigger).await {
            Ok(run) => Ok(run),
            Err(error) => {
                self.engine.lock().await.complete(task_id);
                Err(error)
            }
        }
    }

    async fn enqueue_inner(
        self: &Arc<Self>,
        task: ScheduledTask,
        trigger: &str,
    ) -> Result<TaskRun, cool_store::StoreError> {
        let actor = crate::local_actor();
        let next = scheduler::next_run(&task, crate::legacy::now_seconds())
            .ok()
            .flatten()
            .map(|timestamp| cool_store::python_datetime(timestamp, 0));
        self.store.record_task_fired(task.id, next.as_deref())?;
        let run = self.store.create_task_run(
            &actor.id,
            task.id,
            &NewTaskRun {
                trigger_source: trigger.to_owned(),
                prompt: task.prompt.clone(),
                status: TASK_RUN_RUNNING.to_owned(),
                approval_policy: Some(task.approval_policy.clone()),
                approval_reason: Some(approval_reason(&task.approval_policy)),
                skip_reason: None,
            },
        )?;
        let (cancel_tx, cancel_rx) = watch::channel(None);
        self.live.lock().await.insert(run.id, cancel_tx);
        let executor = Arc::clone(self);
        let task_id = task.id;
        let run_id = run.id;
        tokio::spawn(async move {
            executor.execute(task, run_id, cancel_rx).await;
            executor.live.lock().await.remove(&run_id);
            executor.engine.lock().await.complete(task_id);
        });
        Ok(run)
    }

    async fn execute(
        &self,
        task: ScheduledTask,
        run_id: i64,
        cancel_rx: watch::Receiver<Option<String>>,
    ) {
        let actor = crate::local_actor();
        let started = Instant::now();
        let duration = || started.elapsed().as_millis() as i64;
        // A task that names an unusable working directory fails closed rather
        // than silently running tools against the server workspace.
        let workspace = match task.working_directory.as_deref() {
            Some(path) => match Workspace::new(path) {
                Ok(workspace) => workspace,
                Err(_) => {
                    self.fail_run(
                        &actor,
                        &task,
                        run_id,
                        "invalid working directory",
                        duration(),
                    );
                    return;
                }
            },
            None => self.workspace.clone(),
        };
        let model = task
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let limits = AgentLimits {
            max_iterations: task.max_iterations.clamp(1, i64::from(u32::MAX)) as u32,
            max_cost_micro_usd: task
                .max_cost_per_run
                .filter(|usd| *usd > 0.0)
                .map(|usd| (usd * 1_000_000.0) as u64),
            ..AgentLimits::default()
        };
        let tool_names = task
            .tools_whitelist
            .as_ref()
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            });
        let request = AgentRequest {
            model,
            history: Vec::new(),
            user_input: task.prompt.clone(),
            system_prompt: None,
            mode: Some("task".to_owned()),
            temperature: 0.0,
            max_tokens: None,
            limits,
            tool_names,
            tool_context: ToolContext::new(workspace, task_policy(&self.policy, &task)),
        };
        // The task's approval policy is enforced by the capability policy: a
        // `deny_external` task denies `send_external`, everything else is
        // auto-approved (matching Python's background-run semantics).
        let gate = AutoApprovalGate {
            outcome: ApprovalOutcome::Approved,
        };
        let run = self.runtime.run(
            request,
            &CollectSink,
            &gate,
            CancelSignal::from_receiver(cancel_rx),
        );
        // Python falls back to `scheduler_task_timeout_s` (900 s) when the task
        // has no explicit timeout.
        let timeout_seconds = task
            .timeout_s
            .filter(|seconds| *seconds > 0.0)
            .unwrap_or(DEFAULT_TASK_TIMEOUT_SECONDS);
        let result = match tokio::time::timeout(Duration::from_secs_f64(timeout_seconds), run).await
        {
            Ok(result) => result,
            Err(_) => Ok(RunOutcome::Failed {
                history: Vec::new(),
                code: "task_timeout".to_owned(),
            }),
        };
        let (status, output, error, usage) = match result {
            Ok(RunOutcome::Completed { history, usage }) => (
                TASK_RUN_COMPLETED,
                last_assistant(&history).map(|text| mask_secrets(&text)),
                None,
                Some(usage),
            ),
            Ok(RunOutcome::Cancelled { .. }) => (TASK_RUN_CANCELLED, None, None, None),
            Ok(RunOutcome::Failed { code, .. }) => (TASK_RUN_FAILED, None, Some(code), None),
            Err(error) => (TASK_RUN_FAILED, None, Some(error.to_string()), None),
        };
        let usage_json = usage.and_then(|usage| serde_json::to_value(usage).ok());
        let _ = self.store.finish_task_run(
            &actor.id,
            run_id,
            status,
            output.as_deref(),
            error.as_deref(),
            usage_json.as_ref(),
            Some(duration()),
            None,
            None,
        );
        // Only a hard failure counts against the task's failure ceiling; a
        // cancel is not a failure (matching Python).
        let _ = self.store.record_task_outcome(
            task.id,
            status,
            status != TASK_RUN_FAILED,
            self.config.max_consecutive_failures,
        );
    }

    fn fail_run(
        &self,
        actor: &ActorRef,
        task: &ScheduledTask,
        run_id: i64,
        error: &str,
        duration_ms: i64,
    ) {
        let _ = self.store.finish_task_run(
            &actor.id,
            run_id,
            TASK_RUN_FAILED,
            None,
            Some(error),
            None,
            Some(duration_ms),
            None,
            None,
        );
        let _ = self.store.record_task_outcome(
            task.id,
            TASK_RUN_FAILED,
            false,
            self.config.max_consecutive_failures,
        );
    }
}

/// Narrow the core policy with the task's stored capability policy (a task may
/// only restrict, never widen) and deny `send_external` for `deny_external`.
fn task_policy(base: &CapabilityPolicy, task: &ScheduledTask) -> CapabilityPolicy {
    let mut wildcard = None;
    let mut per_capability = Vec::new();
    if let Some(Value::Object(entries)) = task.capability_policy.as_ref() {
        for (name, value) in entries {
            let Some(decision) = value.as_str().and_then(decision_from_name) else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if name == "*" {
                wildcard = Some(decision);
            } else if let Some(capability) = capability_from_name(&name) {
                per_capability.push((capability, decision));
            }
        }
    }
    let mut child = CapabilityPolicy::new(wildcard);
    for (capability, decision) in per_capability {
        child.set(capability, decision);
    }
    if task.approval_policy != APPROVAL_ALLOW_ALL {
        child.set(Capability::SendExternal, Decision::Deny);
    }
    base.narrow_with(&child)
}

pub(crate) fn capability_from_name(name: &str) -> Option<Capability> {
    match name {
        "read" => Some(Capability::Read),
        "write" => Some(Capability::Write),
        "execute" => Some(Capability::Execute),
        "network" => Some(Capability::Network),
        "git" => Some(Capability::Git),
        "send_external" => Some(Capability::SendExternal),
        _ => None,
    }
}

pub(crate) fn decision_from_name(name: &str) -> Option<Decision> {
    match name.trim().to_ascii_lowercase().as_str() {
        "allow" => Some(Decision::Allow),
        "ask" => Some(Decision::Ask),
        "deny" => Some(Decision::Deny),
        _ => None,
    }
}

/// Terminal assistant text of a run, used as the task-run output.
fn last_assistant(history: &[Message]) -> Option<String> {
    history
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::Assistant)
        .and_then(|message| message.content.clone())
        .filter(|text| !text.is_empty())
}

fn approval_reason(policy: &str) -> String {
    if policy == APPROVAL_ALLOW_ALL {
        "Pre-approved by the user: this background task may use external tools.".to_owned()
    } else {
        "Background run without a human in the loop: tools with an external side effect (send_external) are denied. Set the task's approval policy to allow_all to pre-approve them."
            .to_owned()
    }
}

/// A sink that ignores events. Background task runs persist their result in the
/// `task_runs` table rather than the canonical event log.
struct CollectSink;

#[async_trait]
impl EventSink for CollectSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        Ok(EventEnvelope {
            event_id: "task-event".to_owned(),
            schema_version: V1Version::VALUE,
            session_id: "task".to_owned(),
            run_id: "task".to_owned(),
            item_id: None,
            seq: 0,
            occurred_at: String::new(),
            actor: ActorRef {
                id: "local-user".to_owned(),
                kind: ActorKind::LocalUser,
            },
            source: "task-executor".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: BTreeMap::new(),
        })
    }
}
