//! Foreground subagent executor (M11).
//!
//! Drives `subagents.launch` / `launch_batch` / `runs_cancel` through the Rust
//! agent runtime instead of only creating durable rows. The executor is
//! actor-scoped to the local single-user actor, matching the rest of the local
//! facade; the `server` profile must revisit this when it introduces multiple
//! actors.
//!
//! Durability: the child conversation transcript is written to the legacy
//! `messages` table (the documented subagent exception to "Rust never writes
//! legacy conversations/messages") because the React subagent UI reads the
//! child transcript through `subagents.runs_get`; the child `agent_runs` row
//! links the run. Subagent lifecycle events are projected into the parent
//! conversation's canonical session when that session is linked.
//!
//! Capability policy: a role/profile policy is applied as a narrowing child of
//! the core policy (it can only restrict, never widen). Approval-gated tools
//! are auto-approved because a launched subagent has no interactive approval
//! channel, matching the background-run semantics.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, EventSink, Message,
    MessageRole, RunOutcome, RuntimeError, SubagentRequest, ToolContext,
};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, CanonicalEvent, EventEnvelope, ItemEvent, RunTerminal,
    UsageUpdated, V1Version,
};
use cool_security::{CapabilityPolicy, Workspace, mask_json, mask_secrets};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::StoreError;
use cool_store::domains::conversations::{NewConversation, NewMessage};
use cool_store::domains::runs::NewRun;
use cool_store::domains::subagents::{NewSubagentRun, SubagentRun, TERMINAL_SUBAGENT_STATUSES};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

use crate::scheduler::{capability_from_name, decision_from_name};

/// One launch request, decoupled from the protocol params so the executor does
/// not retain the request envelope.
#[derive(Clone, Debug)]
pub struct SubagentLaunchSpec {
    pub parent_conversation_id: i64,
    pub role_id: Option<i64>,
    pub profile_id: Option<i64>,
    pub parent_run_id: Option<i64>,
    /// Owning `research_runs.id` when the subagent is a deep-research worker.
    pub research_run_id: Option<i64>,
    pub name: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
}

/// Resolved role/profile configuration for one subagent execution (Python
/// `execute_subagent` precedence).
struct ResolvedConfig {
    role_name: String,
    prompt: String,
    model: String,
    system_prompt: Option<String>,
    tool_names: Option<BTreeSet<String>>,
    max_iterations: i64,
    max_cost_usd: Option<f64>,
    capability_policy: Option<Value>,
    working_directory: Option<String>,
    /// Profile-selected process launcher (P0.3): `settings.process_launcher`
    /// + `settings.sandbox_backend`. `COOL_PROCESS_LAUNCHER` and the CLI flag
    ///   still take precedence in `execute`.
    launcher: Option<Arc<dyn cool_agent::ProcessLauncher>>,
}

/// Live cancel channels per `subagent_runs.id`.
type LiveMap = HashMap<i64, watch::Sender<Option<String>>>;

/// Identifiers for one spawned child execution.
#[derive(Clone, Copy)]
struct ChildContext {
    run_id: i64,
    child_run_id: i64,
    child_conversation_id: i64,
    parent_conversation_id: i64,
}

/// Runs launched subagents through the Rust agent runtime.
pub struct SubagentExecutor {
    store: Arc<LegacyStore>,
    durable: DurableStore,
    runtime: AgentRuntime,
    workspace: Workspace,
    policy: CapabilityPolicy,
    default_model: String,
    /// Shared launcher / host env / rule state the children inherit
    /// (subagents narrow the policy but keep the parent's launcher, P0.3).
    host: cool_agent::HostContext,
    /// Live cancel channels per `subagent_runs.id`. A std mutex (never held
    /// across an await) so the cleanup guard can remove the entry from `Drop`
    /// even if the executor task panics.
    live: StdMutex<LiveMap>,
}

impl SubagentExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<LegacyStore>,
        durable: DurableStore,
        runtime: AgentRuntime,
        workspace: Workspace,
        policy: CapabilityPolicy,
        default_model: String,
        host: cool_agent::HostContext,
    ) -> Self {
        Self {
            store,
            durable,
            runtime,
            workspace,
            policy,
            default_model,
            host,
            live: StdMutex::new(HashMap::new()),
        }
    }

    /// The base policy plus the merged project + user rules (P1.6); the
    /// child's `capability_policy` narrows the result at `subagent_policy`.
    fn merged_policy(&self) -> CapabilityPolicy {
        let mut policy = self.policy.clone();
        let mut rules = self.host.rules.project_rules();
        rules.extend(crate::user_policy_rules(
            &self.store,
            &crate::project_key(&self.workspace),
        ));
        policy.set_rules(rules);
        policy
    }

    /// Launch one subagent run. Idempotent on `(actor, key)`: a replay returns
    /// the original run row without creating a second child conversation.
    pub async fn launch(
        self: &Arc<Self>,
        actor_id: &str,
        spec: SubagentLaunchSpec,
        key: &str,
        fingerprint: &str,
    ) -> Result<SubagentRun, StoreError> {
        let outcome = self
            .store
            .run_idempotent_async(actor_id, "subagents.launch", key, fingerprint, || async {
                self.launch_inner(actor_id, spec.clone()).await
            })
            .await?;
        Ok(outcome.value)
    }

    /// Launch a batch of subagent runs. Items are all resolved (role/profile
    /// load + parent ownership) before any row is written, so a bad item does
    /// not leave a partial batch behind; the batch is idempotent on
    /// `(actor, key)`.
    pub async fn launch_batch(
        self: &Arc<Self>,
        actor_id: &str,
        specs: Vec<SubagentLaunchSpec>,
        key: &str,
        fingerprint: &str,
    ) -> Result<Vec<SubagentRun>, StoreError> {
        let outcome = self
            .store
            .run_idempotent_async(
                actor_id,
                "subagents.launch_batch",
                key,
                fingerprint,
                || async {
                    let mut resolved = Vec::with_capacity(specs.len());
                    for spec in &specs {
                        resolved.push(self.resolve(actor_id, spec)?);
                    }
                    let mut runs = Vec::with_capacity(specs.len());
                    for (spec, config) in specs.iter().zip(resolved) {
                        runs.push(self.launch_resolved(actor_id, spec, config).await?);
                    }
                    Ok(runs)
                },
            )
            .await?;
        Ok(outcome.value)
    }

    /// Cancel a subagent run: signal the live execution (if any) and flip the
    /// row. Idempotent on `(actor, key)`.
    pub async fn cancel(
        self: &Arc<Self>,
        actor_id: &str,
        run_id: i64,
        key: &str,
        fingerprint: &str,
    ) -> Result<SubagentRun, StoreError> {
        let outcome = self
            .store
            .run_idempotent_async(
                actor_id,
                "subagents.runs_cancel",
                key,
                fingerprint,
                || async {
                    // Authorize before signalling so a foreign actor cannot
                    // cancel another actor's live run.
                    self.store.get_subagent_run(actor_id, run_id)?;
                    if let Some(sender) = lock_live(&self.live).remove(&run_id) {
                        let _ = sender.send(Some("cancelled".to_owned()));
                    }
                    self.store.cancel_subagent_run(actor_id, run_id)
                },
            )
            .await?;
        Ok(outcome.value)
    }

    /// Current state of a subagent run row (research polling loop).
    pub fn get_run(&self, actor_id: &str, run_id: i64) -> Result<SubagentRun, StoreError> {
        self.store.get_subagent_run(actor_id, run_id)
    }

    /// Signal a live execution to stop without touching the row (internal
    /// callers that already cancelled or own the row, e.g. research cancel).
    pub fn signal_cancel(&self, run_id: i64) {
        if let Some(sender) = lock_live(&self.live).get(&run_id) {
            let _ = sender.send(Some("cancelled".to_owned()));
        }
    }

    /// Poll the row until terminal or `cancel_rx` fires; on cancel the live
    /// sender is signaled once and the row is flipped to `cancelled` (the
    /// executor's finalize step preserves that early terminal). Used by the
    /// `spawn_subagent` tool and the research gather stage.
    pub async fn await_terminal(
        &self,
        actor_id: &str,
        run_id: i64,
        cancel_rx: &mut watch::Receiver<Option<String>>,
    ) -> Result<SubagentRun, StoreError> {
        const POLL: Duration = Duration::from_millis(250);
        let mut signaled = false;
        loop {
            if cancel_rx.borrow().is_some() && !signaled {
                signaled = true;
                self.signal_cancel(run_id);
                let _ = self.store.cancel_subagent_run(actor_id, run_id);
            }
            let run = self.store.get_subagent_run(actor_id, run_id)?;
            if TERMINAL_SUBAGENT_STATUSES.contains(&run.status.as_str()) {
                return Ok(run);
            }
            // `changed()` wakes on cancel; otherwise poll again.
            let _ = tokio::time::timeout(POLL, cancel_rx.changed()).await;
        }
    }

    async fn launch_inner(
        self: &Arc<Self>,
        actor_id: &str,
        spec: SubagentLaunchSpec,
    ) -> Result<SubagentRun, StoreError> {
        let resolved = self.resolve(actor_id, &spec)?;
        self.launch_resolved(actor_id, &spec, resolved).await
    }

    async fn launch_resolved(
        self: &Arc<Self>,
        actor_id: &str,
        spec: &SubagentLaunchSpec,
        resolved: ResolvedConfig,
    ) -> Result<SubagentRun, StoreError> {
        let title = spec.name.clone().unwrap_or_else(|| {
            format!(
                "Subagent: {}",
                spec.prompt.chars().take(40).collect::<String>()
            )
        });
        let child = self.store.create_conversation(
            actor_id,
            &NewConversation {
                title: Some(title),
                model: Some(resolved.model.clone()),
                working_directory: resolved.working_directory.clone(),
                metadata: Some(json!({
                    "is_subagent": true,
                    "parent_conversation_id": spec.parent_conversation_id,
                })),
                ..NewConversation::default()
            },
        )?;
        let child_run = self.store.create_run(
            actor_id,
            child.id,
            &NewRun {
                model: Some(resolved.model.clone()),
                config: Some(json!({
                    "is_subagent": true,
                    "role_id": spec.role_id,
                    "profile_id": spec.profile_id,
                })),
                status: Some("queued".to_owned()),
            },
        )?;
        // Persist the prompt as the child's first message so the transcript is
        // visible while the run is queued/running (Python `create_subagent_run`).
        self.store.add_message(
            actor_id,
            child.id,
            &NewMessage {
                role: "user".to_owned(),
                content: Some(spec.prompt.clone()),
                ..NewMessage::default()
            },
        )?;
        let run = self.store.create_subagent_run(
            actor_id,
            spec.parent_conversation_id,
            &NewSubagentRun {
                role_id: spec.role_id,
                parent_run_id: spec.parent_run_id,
                conversation_id: child.id,
                run_id: Some(child_run.id),
                name: spec.name.clone(),
                prompt: spec.prompt.clone(),
                profile_id: spec.profile_id,
                research_run_id: spec.research_run_id,
            },
        )?;
        let (cancel_tx, cancel_rx) = watch::channel(None);
        lock_live(&self.live).insert(run.id, cancel_tx);
        let executor = Arc::clone(self);
        let context = ChildContext {
            run_id: run.id,
            child_run_id: child_run.id,
            child_conversation_id: child.id,
            parent_conversation_id: spec.parent_conversation_id,
        };
        tokio::spawn(async move {
            // The guard removes the live entry even if `execute` panics.
            let _guard = LiveGuard {
                executor: Arc::clone(&executor),
                run_id: context.run_id,
            };
            executor.execute(context, resolved, cancel_rx).await;
        });
        Ok(run)
    }

    fn resolve(
        &self,
        actor_id: &str,
        spec: &SubagentLaunchSpec,
    ) -> Result<ResolvedConfig, StoreError> {
        let parent = self
            .store
            .get_conversation(actor_id, spec.parent_conversation_id)?;
        let role = match spec.role_id {
            Some(role_id) => Some(self.store.get_subagent_role(role_id)?),
            None => None,
        };
        let profile = match spec.profile_id {
            Some(profile_id) => Some(self.store.get_profile(profile_id)?),
            None => None,
        };
        let model = spec
            .model
            .clone()
            .or_else(|| profile.as_ref().and_then(|profile| profile.model.clone()))
            .or_else(|| role.as_ref().and_then(|role| role.model.clone()))
            .unwrap_or_else(|| self.default_model.clone());
        // Python prefers the profile system prompt when it is truthy, else the
        // role's; an empty string falls through.
        let system_prompt = profile
            .as_ref()
            .and_then(|profile| profile.system_prompt.clone())
            .filter(|prompt| !prompt.is_empty())
            .or_else(|| {
                role.as_ref()
                    .and_then(|role| role.system_prompt.clone())
                    .filter(|prompt| !prompt.is_empty())
            });
        let tool_names = profile
            .as_ref()
            .and_then(|profile| profile.tool_names.clone())
            .or_else(|| role.as_ref().and_then(|role| role.tool_names.clone()))
            .and_then(|value| value.as_array().cloned())
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            });
        let max_iterations = role.as_ref().map_or(10, |role| role.max_iterations);
        let max_cost_usd = role.as_ref().and_then(|role| role.max_cost_usd);
        let role_policy = role
            .as_ref()
            .and_then(|role| role.capability_policy.clone());
        let capability_policy = profile
            .as_ref()
            .and_then(|profile| profile.settings.as_ref())
            .and_then(|settings| settings.get("capability_policy"))
            .cloned()
            .or(role_policy);
        let launcher = cool_agent::launcher_from_profile(
            profile
                .as_ref()
                .and_then(|profile| profile.settings.as_ref()),
        )
        .map_err(cool_store::StoreError::InvalidInput)?;
        Ok(ResolvedConfig {
            role_name: role
                .as_ref()
                .map(|role| role.name.clone())
                .or_else(|| spec.name.clone())
                .unwrap_or_else(|| "subagent".to_owned()),
            prompt: spec.prompt.clone(),
            model,
            system_prompt,
            tool_names,
            max_iterations,
            max_cost_usd,
            capability_policy,
            working_directory: parent.working_directory,
            launcher,
        })
    }

    async fn execute(
        &self,
        context: ChildContext,
        resolved: ResolvedConfig,
        cancel_rx: watch::Receiver<Option<String>>,
    ) {
        let actor = crate::local_actor();
        // A cancel that raced the spawn already flipped the row terminal; do not
        // resurrect it as running.
        if self
            .store
            .mark_subagent_run_running(&actor.id, context.run_id)
            .is_err()
        {
            return;
        }
        // A subagent that names an unusable working directory fails closed
        // rather than silently running tools against the server workspace.
        let workspace = match resolved.working_directory.as_deref() {
            Some(path) => match Workspace::new(path) {
                Ok(workspace) => workspace,
                Err(_) => {
                    self.fail_run(&actor, context, "invalid working directory");
                    return;
                }
            },
            None => self.workspace.clone(),
        };
        let limits = AgentLimits {
            max_iterations: resolved.max_iterations.clamp(1, i64::from(u32::MAX)) as u32,
            max_cost_micro_usd: resolved
                .max_cost_usd
                .filter(|usd| *usd > 0.0)
                .map(|usd| (usd * 1_000_000.0) as u64),
            ..AgentLimits::default()
        };
        // Launcher chain (P0.3): env → profile → executor default (flag or
        // disabled). An invalid env value fails the child closed.
        let launcher = match cool_agent::launcher_from_env() {
            Ok(Some(launcher)) => launcher,
            Ok(None) => resolved
                .launcher
                .clone()
                .unwrap_or_else(|| self.host.launcher.clone()),
            Err(error) => {
                self.fail_run(&actor, context, &error);
                return;
            }
        };
        let request = AgentRequest {
            model: resolved.model.clone(),
            history: Vec::new(),
            user_input: resolved.prompt.clone(),
            system_prompt: resolved.system_prompt.clone(),
            mode: Some("subagent".to_owned()),
            temperature: 0.0,
            max_tokens: None,
            limits,
            tool_names: resolved.tool_names.clone(),
            tool_context: ToolContext::new(
                workspace,
                subagent_policy(&self.merged_policy(), resolved.capability_policy.as_ref()),
            )
            .with_actor(crate::local_actor().id)
            .with_launcher(launcher)
            .with_environment(self.host.environment.clone())
            .with_session_rules(self.host.rules.session_rules(&context.run_id.to_string())),
        };
        let child_sink = LegacyTranscriptSink {
            store: Arc::clone(&self.store),
            actor_id: actor.id.clone(),
            conversation_id: context.child_conversation_id,
            model: resolved.model.clone(),
            reasoning: StdMutex::new(String::new()),
            usage: StdMutex::new(None),
        };
        // The parent session is projected best-effort: a conversation that was
        // never linked to a canonical session has no run to append to.
        let lifecycle = self.begin_lifecycle(&actor.id, context.parent_conversation_id);
        let disabled = NullSink;
        let lifecycle_sink = ParentLifecycleSink {
            store: self.durable.clone(),
            actor_id: actor.id.clone(),
            session_id: lifecycle.as_ref().map(|(session, _)| session.clone()),
            run_id: lifecycle.as_ref().map(|(_, run)| run.clone()),
        };
        let parent_sink: &dyn EventSink = if lifecycle.is_some() {
            &lifecycle_sink
        } else {
            &disabled
        };
        let subagent_request = SubagentRequest {
            run_id: context.run_id.to_string(),
            role: resolved.role_name.clone(),
            agent: request,
            cancel: CancelSignal::from_receiver(cancel_rx),
        };
        let outcome = self
            .runtime
            .run_subagent(
                subagent_request,
                parent_sink,
                &child_sink,
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
            )
            .await;
        let (status, summary, error, usage_json) = match &outcome {
            Ok(RunOutcome::Completed { history, usage }) => (
                "completed",
                last_assistant(history).map(|text| mask_secrets(&text)),
                None,
                serde_json::to_value(usage).ok(),
            ),
            Ok(RunOutcome::Cancelled { .. }) => ("cancelled", None, None, None),
            Ok(RunOutcome::Failed { code, .. }) => ("failed", None, Some(mask_secrets(code)), None),
            Err(error) => ("failed", None, Some(mask_secrets(&error.to_string())), None),
        };
        let usage = usage_json.as_ref();
        // `finalize_*` preserves an earlier cancellation rather than
        // overwriting it with a late completion. The returned row's status is
        // authoritative, so the child run and the parent lifecycle run agree
        // with the `subagent_runs` outcome even when a cancel won the race.
        let finalized = self.store.finalize_subagent_run(
            &actor.id,
            context.run_id,
            status,
            summary.as_deref(),
            usage,
            error.as_deref(),
        );
        let effective = finalized
            .as_ref()
            .map(|run| run.status.as_str())
            .unwrap_or(status);
        let _ = self.store.finish_run(
            &actor.id,
            context.child_run_id,
            effective,
            usage,
            None,
            Some(effective),
            error.as_deref(),
        );
        self.close_lifecycle(&actor.id, lifecycle, effective, error.as_deref());
    }

    /// Create a canonical lifecycle run for the parent session, if linked.
    fn begin_lifecycle(
        &self,
        actor_id: &str,
        parent_conversation_id: i64,
    ) -> Option<(String, String)> {
        let session_id = self
            .durable
            .session_for_conversation(actor_id, parent_conversation_id)
            .ok()??;
        let run_id = self
            .durable
            .start_auxiliary_run(actor_id, &session_id)
            .ok()?;
        Some((session_id, run_id))
    }

    fn close_lifecycle(
        &self,
        actor_id: &str,
        lifecycle: Option<(String, String)>,
        status: &str,
        error: Option<&str>,
    ) {
        let Some((session_id, run_id)) = lifecycle else {
            return;
        };
        let event = match status {
            "completed" => CanonicalEvent::RunCompleted(RunTerminal {
                reason: "subagent_completed".to_owned(),
                error_code: None,
            }),
            "cancelled" => CanonicalEvent::RunCancelled(RunTerminal {
                reason: "subagent_cancelled".to_owned(),
                error_code: None,
            }),
            _ => CanonicalEvent::RunFailed(RunTerminal {
                reason: "subagent_failed".to_owned(),
                error_code: error.map(str::to_owned),
            }),
        };
        let envelope = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id,
            run_id,
            item_id: None,
            seq: 0,
            occurred_at: String::new(),
            actor: ActorRef {
                id: "cool-agent".to_owned(),
                kind: ActorKind::System,
            },
            source: "subagent-executor".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: Default::default(),
        };
        let _ = self.durable.append_event_auto(actor_id, envelope);
    }

    fn fail_run(&self, actor: &ActorRef, context: ChildContext, error: &str) {
        // `finalize_*` preserves a cancellation delivered before this point.
        let _ = self.store.finalize_subagent_run(
            &actor.id,
            context.run_id,
            "failed",
            None,
            None,
            Some(error),
        );
        let _ = self.store.finish_run(
            &actor.id,
            context.child_run_id,
            "failed",
            None,
            None,
            Some("failed"),
            Some(error),
        );
    }
}

/// Lock the live map, recovering from a poisoned mutex instead of panicking.
fn lock_live(live: &StdMutex<LiveMap>) -> std::sync::MutexGuard<'_, LiveMap> {
    live.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Removes a run's live cancel entry on drop, including when the executor task
/// panics.
struct LiveGuard {
    executor: Arc<SubagentExecutor>,
    run_id: i64,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        lock_live(&self.executor.live).remove(&self.run_id);
        // A panicking executor task would otherwise leave the row `running`
        // forever; the store-level finalize preserves an earlier terminal
        // status, so this only fires on abnormal termination.
        let actor = crate::local_actor();
        if let Ok(run) = self.executor.store.get_subagent_run(&actor.id, self.run_id)
            && !TERMINAL_SUBAGENT_STATUSES.contains(&run.status.as_str())
        {
            let _ = self.executor.store.finalize_subagent_run(
                &actor.id,
                self.run_id,
                "failed",
                None,
                None,
                Some("executor terminated abnormally"),
            );
        }
    }
}

/// Narrow the core policy with a role/profile capability policy (a subagent may
/// only restrict, never widen).
fn subagent_policy(base: &CapabilityPolicy, policy: Option<&Value>) -> CapabilityPolicy {
    let Some(Value::Object(entries)) = policy else {
        return base.clone();
    };
    let mut wildcard = None;
    let mut per_capability = Vec::new();
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
    let mut child = CapabilityPolicy::new(wildcard);
    for (capability, decision) in per_capability {
        child.set(capability, decision);
    }
    base.narrow_with(&child)
}

/// Mask secrets inside a JSON value before it reaches the legacy store.
fn masked_value(mut value: Value) -> Value {
    mask_json(&mut value);
    value
}

/// Terminal assistant text of a run, used as the subagent result summary.
fn last_assistant(history: &[Message]) -> Option<String> {
    history
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::Assistant)
        .and_then(|message| message.content.clone())
        .filter(|text| !text.is_empty())
}

/// Writes the child conversation transcript to the legacy `messages` table.
struct LegacyTranscriptSink {
    store: Arc<LegacyStore>,
    actor_id: String,
    conversation_id: i64,
    model: String,
    reasoning: StdMutex<String>,
    usage: StdMutex<Option<UsageUpdated>>,
}

impl LegacyTranscriptSink {
    fn add(&self, message: NewMessage) -> Result<(), RuntimeError> {
        self.store
            .add_message(&self.actor_id, self.conversation_id, &message)
            .map(|_| ())
            .map_err(|error| RuntimeError::Sink(error.to_string()))
    }
}

#[async_trait]
impl EventSink for LegacyTranscriptSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        match event {
            // The prompt is persisted at launch, so the runtime's echo of it is
            // not written a second time.
            CanonicalEvent::ItemCompleted(item) if item.role.as_deref() == Some("user") => {}
            CanonicalEvent::ItemCompleted(item) if item.role.as_deref() == Some("assistant") => {
                let thinking = self
                    .reasoning
                    .lock()
                    .ok()
                    .map(|mut text| std::mem::take(&mut *text))
                    .filter(|text| !text.is_empty());
                let usage = self.usage.lock().ok().and_then(|mut usage| usage.take());
                let tool_calls = item
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.call_id,
                            "name": call.name,
                            "arguments": call.arguments,
                        })
                    })
                    .collect::<Vec<_>>();
                self.add(NewMessage {
                    role: "assistant".to_owned(),
                    content: item.content.as_deref().map(mask_secrets),
                    tool_calls: (!tool_calls.is_empty()).then(|| masked_value(json!(tool_calls))),
                    thinking: thinking.map(|text| mask_secrets(&text)),
                    model: Some(self.model.clone()),
                    usage: usage
                        .as_ref()
                        .and_then(|usage| serde_json::to_value(usage).ok()),
                    ..NewMessage::default()
                })?;
            }
            CanonicalEvent::ReasoningDelta(delta) => {
                if let Ok(mut reasoning) = self.reasoning.lock() {
                    reasoning.push_str(&delta.text);
                }
            }
            CanonicalEvent::UsageUpdated(usage) => {
                if let Ok(mut pending) = self.usage.lock() {
                    *pending = Some(usage);
                }
            }
            CanonicalEvent::ToolCompleted(tool) => {
                self.add(NewMessage {
                    role: "tool".to_owned(),
                    tool_result: Some(masked_value(json!({
                        "tool_call_id": tool.call_id,
                        "name": tool.name,
                        "result": tool.result,
                    }))),
                    ..NewMessage::default()
                })?;
            }
            CanonicalEvent::ToolFailed(tool) => {
                self.add(NewMessage {
                    role: "tool".to_owned(),
                    content: tool.message.as_deref().map(mask_secrets),
                    tool_result: Some(masked_value(json!({
                        "tool_call_id": tool.call_id,
                        "name": tool.name,
                        "result": {
                            "is_error": true,
                            "error": tool.message,
                            "error_code": tool.error_code,
                        },
                    }))),
                    ..NewMessage::default()
                })?;
            }
            _ => {}
        }
        Ok(synthetic_envelope())
    }
}

/// Projects subagent lifecycle events into the parent conversation's canonical
/// session.
struct ParentLifecycleSink {
    store: DurableStore,
    actor_id: String,
    session_id: Option<String>,
    run_id: Option<String>,
}

#[async_trait]
impl EventSink for ParentLifecycleSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        let (Some(session_id), Some(run_id)) = (self.session_id.clone(), self.run_id.clone())
        else {
            return Ok(synthetic_envelope());
        };
        let envelope = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id,
            run_id,
            item_id: None,
            seq: 0,
            occurred_at: String::new(),
            actor: ActorRef {
                id: "cool-agent".to_owned(),
                kind: ActorKind::System,
            },
            source: "subagent-executor".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: Default::default(),
        };
        self.store
            .append_event_auto(&self.actor_id, envelope)
            .map_err(|error| RuntimeError::Sink(error.to_string()))
    }
}

/// A sink for conversations with no linked canonical session.
struct NullSink;

#[async_trait]
impl EventSink for NullSink {
    async fn emit(&self, _event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        Ok(synthetic_envelope())
    }
}

fn synthetic_envelope() -> EventEnvelope {
    EventEnvelope {
        event_id: "subagent-event".to_owned(),
        schema_version: V1Version::VALUE,
        session_id: "subagent".to_owned(),
        run_id: "subagent".to_owned(),
        item_id: None,
        seq: 0,
        occurred_at: String::new(),
        actor: ActorRef {
            id: "local-user".to_owned(),
            kind: ActorKind::LocalUser,
        },
        source: "subagent-executor".to_owned(),
        causation_id: None,
        correlation_id: None,
        event: CanonicalEvent::ItemCompleted(ItemEvent {
            role: None,
            content: None,
            tool_calls: Vec::new(),
        }),
        extensions: Default::default(),
    }
}
