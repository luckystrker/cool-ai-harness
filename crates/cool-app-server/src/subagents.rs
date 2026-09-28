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
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, EventSink, LaunchSpec,
    Message, MessageRole, NetAccess, ResourceLimits, RunOutcome, RuntimeError, SubagentRequest,
    ToolContext,
};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, CanonicalEvent, EventEnvelope, ItemEvent, RunTerminal,
    UsageUpdated, V1Version,
};
use cool_security::{
    CapabilityPolicy, PolicyRule, Workspace, mask_json, mask_secrets, sanitize_environment,
};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::StoreError;
use cool_store::domains::conversations::{MessagePage, NewConversation, NewMessage};
use cool_store::domains::runs::NewRun;
use cool_store::domains::subagents::{NewSubagentRun, SubagentRun, TERMINAL_SUBAGENT_STATUSES};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

use crate::scheduler::{capability_from_name, decision_from_name};

/// How much of the spawning run's context a child inherits (P1.7).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ForkContext {
    /// Fresh context — the child sees only its prompt.
    #[default]
    None,
    /// A model-written summary of the parent's transcript, folded into the
    /// child's system prompt.
    Summary,
    /// The parent's full transcript becomes the child's starting history.
    Full,
}

impl ForkContext {
    pub fn parse(value: Option<&str>) -> Result<Self, StoreError> {
        match value.unwrap_or("none") {
            "none" => Ok(Self::None),
            "summary" => Ok(Self::Summary),
            "full" => Ok(Self::Full),
            other => Err(StoreError::InvalidInput(format!(
                "unknown fork_context '{other}' (expected none|summary|full)"
            ))),
        }
    }
}

/// Whether the child shares the parent's workspace or gets a git worktree
/// of its own (P1.7).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SubagentIsolation {
    #[default]
    Shared,
    /// `git worktree add .cool/worktrees/{run_id} -b cool/sub/{run_id}` inside
    /// the parent workspace via the configured process launcher.
    Worktree,
}

impl SubagentIsolation {
    pub fn parse(value: Option<&str>) -> Result<Self, StoreError> {
        match value.unwrap_or("shared") {
            "shared" => Ok(Self::Shared),
            "worktree" => Ok(Self::Worktree),
            other => Err(StoreError::InvalidInput(format!(
                "unknown isolation '{other}' (expected shared|worktree)"
            ))),
        }
    }
}

/// One launch request, decoupled from the protocol params so the executor does
/// not retain the request envelope.
#[derive(Clone, Debug, Default)]
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
    /// Context forking requested by the parent (P1.7).
    pub fork_context: ForkContext,
    /// Workspace isolation requested by the parent (P1.7).
    pub isolation: SubagentIsolation,
    /// Depth of the spawning run (`ToolContext.spawn_depth`); the child runs
    /// at `spawn_depth + 1` and cannot spawn at `cool_agent::MAX_SPAWN_DEPTH`.
    pub spawn_depth: u32,
    /// Parent transcript snapshot for `fork_context` full/summary — the live
    /// history the runtime refreshed before the tool batch ran.
    pub parent_history: Vec<Message>,
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
    /// Session rules seeded from profile `settings["exec_rules"]` before the
    /// run starts (P2.15 reviewer preset narrows `git` to diff/log this way).
    exec_rules: Vec<PolicyRule>,
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
    /// Row id of the persisted launch prompt; steer drain baselines here so a
    /// `send_to_subagent` accepted while the run is still `queued` lands
    /// above the cursor instead of being dropped (P1.7).
    prompt_message_id: i64,
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

    /// The base capability policy; the child's `capability_policy` narrows
    /// it at `subagent_policy`. Project + user rules ride the live
    /// `rule_source` attached per run instead — for the subagent's own
    /// working directory, not the server's (P1.6).
    fn merged_policy(&self) -> CapabilityPolicy {
        self.policy.clone()
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
        let prompt_message = self.store.add_message(
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
            prompt_message_id: prompt_message.id,
        };
        let spec = spec.clone();
        tokio::spawn(async move {
            // The guard removes the live entry even if `execute` panics.
            let _guard = LiveGuard {
                executor: Arc::clone(&executor),
                run_id: context.run_id,
            };
            executor.execute(context, resolved, spec, cancel_rx).await;
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
        let exec_rules = profile
            .as_ref()
            .and_then(|profile| profile.settings.as_ref())
            .and_then(|settings| settings.get("exec_rules"))
            .and_then(|value| serde_json::from_value::<Vec<PolicyRule>>(value.clone()).ok())
            .unwrap_or_default();
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
            exec_rules,
            working_directory: parent.working_directory,
            launcher,
        })
    }

    async fn execute(
        &self,
        context: ChildContext,
        resolved: ResolvedConfig,
        spec: SubagentLaunchSpec,
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
        // Launcher chain (P0.3): env → profile → executor default (flag or
        // disabled). An invalid env value fails the child closed. Resolved
        // before the workspace because a worktree isolation launch uses it.
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
        // A subagent that names an unusable working directory fails closed
        // rather than silently running tools against the server workspace.
        let mut workspace = match resolved.working_directory.as_deref() {
            Some(path) => match Workspace::new(path) {
                Ok(workspace) => workspace,
                Err(_) => {
                    self.fail_run(&actor, context, "invalid working directory");
                    return;
                }
            },
            None => self.workspace.clone(),
        };
        // isolation=worktree (P1.7): the child edits a git worktree of its own
        // so parallel siblings cannot collide on the shared checkout.
        // Keep the pre-isolation workspace: it is the checkout the worktree
        // was created from — possibly a conversation cwd, not the server's —
        // and teardown needs it for `git worktree`/`branch -D` cleanup.
        let parent_workspace = workspace.clone();
        if spec.isolation == SubagentIsolation::Worktree {
            match create_worktree(
                launcher.as_ref(),
                &workspace,
                &self.host.environment,
                context.run_id,
            )
            .await
            {
                Ok(directory) => match Workspace::new(&directory) {
                    Ok(worktree) => workspace = worktree,
                    Err(_) => {
                        remove_worktree(
                            launcher.as_ref(),
                            &workspace,
                            &self.host.environment,
                            context.run_id,
                        )
                        .await;
                        self.fail_run(
                            &actor,
                            context,
                            "worktree created but is not a usable workspace",
                        );
                        return;
                    }
                },
                Err(error) => {
                    self.fail_run(&actor, context, &error);
                    return;
                }
            }
        }
        // fork_context (P1.7): `full` starts the child on the parent's
        // transcript; `summary` folds a model-written digest of it into the
        // child's system prompt.
        let (history, system_prompt) = match spec.fork_context {
            ForkContext::Full => {
                let mut history = spec.parent_history.clone();
                // The snapshot ends mid-turn: the assistant message that
                // spawned this child carries tool calls whose results do not
                // exist yet — providers reject that transcript.
                close_open_tool_calls(&mut history);
                if let Some(prompt) = &resolved.system_prompt {
                    // The forked transcript carries the parent's system
                    // message, so the runtime's "history already has System"
                    // guard would silently drop the child's own persona —
                    // and `compact_history` keeps only the FIRST system
                    // message, so a second one would be lost on compaction
                    // too. Fold the child's persona into the parent's first
                    // system message so both survive.
                    match history.first_mut() {
                        Some(message) if message.role == MessageRole::System => {
                            let parent = message.content.take().unwrap_or_default();
                            message.content =
                                Some(format!("{prompt}\n\n[Parent instructions]\n{parent}"));
                        }
                        _ => history.insert(0, Message::text(MessageRole::System, prompt.clone())),
                    }
                }
                (history, None)
            }
            ForkContext::Summary => {
                let summary = self
                    .summarize_parent(&workspace, &spec.parent_history, &resolved.model)
                    .await;
                match summary {
                    Some(summary) => (
                        Vec::new(),
                        Some(
                            format!(
                                "{}\n\n[Parent conversation summary — forked context]\n{summary}",
                                resolved.system_prompt.as_deref().unwrap_or_default()
                            )
                            .trim()
                            .to_owned(),
                        ),
                    ),
                    None => (Vec::new(), resolved.system_prompt.clone()),
                }
            }
            ForkContext::None => (Vec::new(), resolved.system_prompt.clone()),
        };
        let limits = AgentLimits {
            max_iterations: resolved.max_iterations.clamp(1, i64::from(u32::MAX)) as u32,
            max_cost_micro_usd: resolved
                .max_cost_usd
                .filter(|usd| *usd > 0.0)
                .map(|usd| (usd * 1_000_000.0) as u64),
            ..AgentLimits::default()
        };
        let request = AgentRequest {
            model: resolved.model.clone(),
            history,
            user_input: resolved.prompt.clone(),
            system_prompt,
            mode: Some("subagent".to_owned()),
            temperature: 0.0,
            max_tokens: None,
            limits,
            tool_names: resolved.tool_names.clone(),
            tool_context: ToolContext::new(
                workspace.clone(),
                subagent_policy(&self.merged_policy(), resolved.capability_policy.as_ref()),
            )
            .with_actor(crate::local_actor().id)
            .with_launcher(launcher.clone())
            .with_environment(self.host.environment.clone())
            .with_session_rules(self.host.rules.session_rules(&context.run_id.to_string()))
            // Nested `spawn_subagent` scopes to this child's conversation —
            // without it `SpawnSubagent` falls back to conversation 1.
            .with_conversation(Some(context.child_conversation_id))
            .with_spawn_depth(spec.spawn_depth.saturating_add(1))
            .with_rule_source(crate::rule_source_for(
                Some(self.store.clone()),
                workspace.clone(),
                self.host.rules.clone(),
            )),
        };
        let child_sink = LegacyTranscriptSink {
            store: Arc::clone(&self.store),
            actor_id: actor.id.clone(),
            conversation_id: context.child_conversation_id,
            model: resolved.model.clone(),
            reasoning: StdMutex::new(String::new()),
            usage: StdMutex::new(None),
            // Steers (`send_to_subagent`) are user messages appended after
            // launch; baseline at the persisted launch prompt so a steer
            // queued before this task ran is still drained — it carries an id
            // above the prompt's — while the prompt itself is never
            // re-delivered as a steer.
            steer_cursor: AtomicI64::new(context.prompt_message_id),
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
        // Profile `exec_rules` seed the run's session rules — evaluated
        // before the capability fallback (P2.15 reviewer keeps `git` to
        // read-only subcommands this way).
        for rule in &resolved.exec_rules {
            self.host
                .rules
                .add_session_rule(&context.run_id.to_string(), rule.clone());
        }
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
        // The run is settled — drop its session-rule set so finished runs
        // don't accumulate per-run state forever.
        self.host.rules.remove_session(&context.run_id.to_string());
        // The worktree Workspace's `cap_std::fs::Dir` is an open handle on
        // `.cool/worktrees/{id}` — while it lives the directory cannot be
        // deleted on Windows (os error 32). The clones held by ToolContext
        // and the rule source were consumed by `run`; drop this one before
        // teardown.
        drop(workspace);
        // Worktree children edit an isolated checkout — preserve their work
        // instead of deleting it: commit any dirty state onto `cool/sub/{id}`,
        // remove only the checkout, and surface the branch name in the run
        // summary so the parent can merge or cherry-pick (P1.7). When the
        // commit fails and the checkout is still dirty, keep it — deleting it
        // would erase the child's work.
        let worktree_note = if spec.isolation == SubagentIsolation::Worktree {
            let clean = commit_worktree_changes(
                launcher.as_ref(),
                &parent_workspace,
                &self.host.environment,
                context.run_id,
            )
            .await;
            if clean {
                remove_worktree(
                    launcher.as_ref(),
                    &parent_workspace,
                    &self.host.environment,
                    context.run_id,
                )
                .await;
                Some(format!(
                    "edits preserved on branch `cool/sub/{}` — merge or cherry-pick into the parent checkout",
                    context.run_id
                ))
            } else {
                Some(format!(
                    "edits could not be committed — checkout kept at `.cool/worktrees/{0}` (branch `cool/sub/{0}` may be incomplete)",
                    context.run_id
                ))
            }
        } else {
            None
        };
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
        let summary = match (&summary, &worktree_note) {
            (text, Some(note)) => Some(format!(
                "{}\n\n[{note}]",
                text.as_deref().unwrap_or_default()
            )),
            (text, None) => text.clone(),
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
    /// Legacy `messages.id` cursor for `drain_steers` (P1.7): rows past it are
    /// pending `send_to_subagent` steers.
    steer_cursor: AtomicI64,
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

    async fn drain_steers(&self) -> Result<Vec<Message>, RuntimeError> {
        // Mirror of the canonical sink's steer drain: `send_to_subagent`
        // appends legacy user messages to the child conversation; anything
        // newer than the cursor becomes a steer for the next iteration.
        let page = self
            .store
            .list_messages(
                &self.actor_id,
                self.conversation_id,
                &MessagePage {
                    before_id: None,
                    after_id: Some(self.steer_cursor.load(Ordering::SeqCst)),
                    limit: Some(100),
                },
            )
            .map_err(|error| RuntimeError::Sink(error.to_string()))?;
        let mut max_id = self.steer_cursor.load(Ordering::SeqCst);
        let mut steers = Vec::new();
        for message in page {
            max_id = max_id.max(message.id);
            if message.role == "user"
                && let Some(content) = message.content
            {
                steers.push(Message::text(MessageRole::User, content));
            }
        }
        self.steer_cursor.fetch_max(max_id, Ordering::SeqCst);
        Ok(steers)
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

/// `isolation=worktree` (P1.7): `git worktree add .cool/worktrees/{run_id}`
/// through the configured process launcher — the same gate shell tools spawn
/// through, so a disabled launcher fails closed rather than spawning `git`
/// unsandboxed.
async fn create_worktree(
    launcher: &dyn cool_agent::ProcessLauncher,
    workspace: &Workspace,
    environment: &HashMap<String, String>,
    run_id: i64,
) -> Result<std::path::PathBuf, String> {
    let root = workspace.root();
    let base = root.join(".cool").join("worktrees");
    std::fs::create_dir_all(&base)
        .map_err(|error| format!("cannot create {}: {error}", base.display()))?;
    let directory = base.join(run_id.to_string());
    let branch = format!("cool/sub/{run_id}");
    // `Workspace::root()` is canonicalized — on Windows that's a verbatim
    // `\\?\` path that git itself refuses when writing the worktree's `.git`
    // pointer file, so hand git the plain path.
    let directory_arg = directory
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .map(str::to_owned)
        .unwrap_or_else(|| directory.to_string_lossy().into_owned());
    let args = vec![
        "worktree".to_owned(),
        "add".to_owned(),
        directory_arg,
        "-b".to_owned(),
        branch,
    ];
    let spec = LaunchSpec {
        cwd: root.to_path_buf(),
        // Same secret filtering the process tools apply — git hooks and
        // credential helpers must not see host tokens.
        env: sanitize_environment(
            environment
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            &BTreeSet::new(),
        )
        .into_iter()
        .collect(),
        stdin: None,
        // `git worktree` is local-only; `Full` is the level every launcher
        // backend accepts (HostLauncher fails closed below it).
        net: NetAccess::Full,
        limits: ResourceLimits {
            timeout: Duration::from_secs(60),
            max_output_bytes: 1 << 20,
        },
    };
    let mut child = launcher
        .spawn("git", &args, &spec)
        .map_err(|error| format!("cannot launch git worktree add: {error}"))?;
    // Both pipes are drained before `wait`: a child that fills an unclaimed
    // pipe buffer would deadlock otherwise.
    let mut stderr_bytes = Vec::new();
    if let Some(mut stderr) = child.stderr().take() {
        use tokio::io::AsyncReadExt as _;
        let _ = stderr.read_to_end(&mut stderr_bytes).await;
    }
    if let Some(mut stdout) = child.stdout().take() {
        use tokio::io::AsyncReadExt as _;
        let mut stdout_bytes = Vec::new();
        let _ = stdout.read_to_end(&mut stdout_bytes).await;
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("git worktree add did not finish: {error}"))?;
    if status.success() {
        Ok(directory)
    } else {
        Err(format!(
            "git worktree add exited with {status}: {}",
            String::from_utf8_lossy(&stderr_bytes).trim()
        ))
    }
}

/// `isolation=worktree` handoff: commits the child's dirty state onto its
/// `cool/sub/{id}` branch so teardown deletes only the checkout, not the
/// work. `--no-verify` skips repo hooks for the same reason the environment
/// is sanitized. Returns whether the checkout ended up clean — the caller
/// keeps the checkout instead of deleting it when `false`.
async fn commit_worktree_changes(
    launcher: &dyn cool_agent::ProcessLauncher,
    workspace: &Workspace,
    environment: &HashMap<String, String>,
    run_id: i64,
) -> bool {
    let directory = workspace
        .root()
        .join(".cool")
        .join("worktrees")
        .join(run_id.to_string());
    if !directory.exists() {
        return true;
    }
    let env: Vec<(String, String)> = sanitize_environment(
        environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        &BTreeSet::new(),
    )
    .into_iter()
    .collect();
    let staged = run_git(
        launcher,
        &directory,
        &env,
        &["add".to_owned(), "-A".to_owned()],
    )
    .await;
    if matches!(staged, Some((true, _))) {
        let _ = run_git(
            launcher,
            &directory,
            &env,
            &[
                "-c".to_owned(),
                "user.name=cool-subagent".to_owned(),
                "-c".to_owned(),
                "user.email=cool-subagent@local".to_owned(),
                "commit".to_owned(),
                "--no-verify".to_owned(),
                "-m".to_owned(),
                format!("cool subagent {run_id} edits"),
            ],
        )
        .await;
    }
    // `status --porcelain` is the source of truth: it reports a clean tree
    // whether the child committed everything itself, our commit succeeded,
    // or there was nothing to commit — and reports leftovers whenever any
    // of those steps failed.
    match run_git(
        launcher,
        &directory,
        &env,
        &["status".to_owned(), "--porcelain".to_owned()],
    )
    .await
    {
        Some((true, stdout)) => stdout.trim().is_empty(),
        _ => false,
    }
}

/// Runs `git <args>` in `cwd` through the launcher with both pipes drained
/// (a child that fills an unclaimed pipe buffer would deadlock on `wait`).
/// Returns `(success, stdout)`; spawn/wait failures return `None`.
async fn run_git(
    launcher: &dyn cool_agent::ProcessLauncher,
    cwd: &std::path::Path,
    env: &[(String, String)],
    args: &[String],
) -> Option<(bool, String)> {
    let spec = LaunchSpec {
        cwd: cwd.to_path_buf(),
        env: env.to_vec(),
        stdin: None,
        net: NetAccess::Full,
        limits: ResourceLimits {
            timeout: Duration::from_secs(60),
            max_output_bytes: 1 << 20,
        },
    };
    let mut child = launcher.spawn("git", args, &spec).ok()?;
    use tokio::io::AsyncReadExt as _;
    let mut stdout_bytes = Vec::new();
    if let Some(mut stdout) = child.stdout().take() {
        let _ = stdout.read_to_end(&mut stdout_bytes).await;
    }
    if let Some(mut stderr) = child.stderr().take() {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes).await;
    }
    let status = child.wait().await.ok()?;
    Some((
        status.success(),
        String::from_utf8_lossy(&stdout_bytes).into_owned(),
    ))
}

/// Best-effort teardown of a worktree-isolated child: delete the checkout
/// (`git worktree remove` is skipped — it path-matches the registration and
/// 8.3/verbatim spellings of the same directory never match on Windows),
/// then `git worktree prune` for the bookkeeping. The `cool/sub/{id}` branch
/// is kept deliberately — it carries the child's edits for the parent to
/// merge or cherry-pick. Cleanup failures only leave litter under
/// `.cool/worktrees/` — never fail the run.
async fn remove_worktree(
    launcher: &dyn cool_agent::ProcessLauncher,
    workspace: &Workspace,
    environment: &HashMap<String, String>,
    run_id: i64,
) {
    let root = workspace.root();
    let directory = root
        .join(".cool")
        .join("worktrees")
        .join(run_id.to_string());
    // The caller drops the child's `Workspace` (a `cap_std` handle on this
    // directory) before teardown; a short retry still covers handles that
    // release asynchronously elsewhere on Windows.
    for _ in 0..50 {
        match std::fs::remove_dir_all(&directory) {
            Ok(_) => break,
            Err(_) if !directory.exists() => break,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    let args = vec!["worktree".to_owned(), "prune".to_owned()];
    let spec = LaunchSpec {
        cwd: root.to_path_buf(),
        env: sanitize_environment(
            environment
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            &BTreeSet::new(),
        )
        .into_iter()
        .collect(),
        stdin: None,
        net: NetAccess::Full,
        limits: ResourceLimits {
            timeout: Duration::from_secs(60),
            max_output_bytes: 1 << 20,
        },
    };
    if let Ok(mut child) = launcher.spawn("git", &args, &spec) {
        use tokio::io::AsyncReadExt as _;
        // Drain both pipes before `wait` so a chatty git cannot deadlock
        // on a full buffer.
        if let Some(mut stderr) = child.stderr().take() {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
        }
        if let Some(mut stdout) = child.stdout().take() {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes).await;
        }
        let _ = child.wait().await;
    }
}

/// Closes assistant tool calls that have no result message — the
/// `fork_context=full` snapshot ends mid-batch, while providers reject
/// transcripts with unanswered calls.
fn close_open_tool_calls(history: &mut Vec<Message>) {
    let answered: BTreeSet<&str> = history
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    let pending: Vec<String> = history
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .flat_map(|message| message.tool_calls.iter())
        .filter(|call| !answered.contains(call.call_id.as_str()))
        .map(|call| call.call_id.clone())
        .collect();
    for call_id in pending {
        let mut message = Message::text(
            MessageRole::Tool,
            "result unavailable — the context forked while this call was in flight",
        );
        message.tool_call_id = Some(call_id);
        history.push(message);
    }
}

/// Renders the parent transcript for `fork_context=summary`, capped so a long
/// run cannot blow the summarizer's input budget.
fn render_parent_transcript(history: &[Message]) -> String {
    let mut output = String::new();
    for message in history {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        if let Some(content) = &message.content {
            for line in content.lines().take(40) {
                output.push_str(&format!("{role}: {line}\n"));
            }
        }
        for call in &message.tool_calls {
            let arguments = serde_json::to_string(&call.arguments).unwrap_or_default();
            output.push_str(&format!("{role}: [tool call {}({arguments})]\n", call.name));
        }
        if output.len() > 48_000 {
            break;
        }
    }
    output
}

impl SubagentExecutor {
    /// `fork_context=summary` (P1.7): a one-iteration model call that digests
    /// the parent transcript — the inline variant of the P0.4 summarizer the
    /// canonical sink uses for compaction.
    async fn summarize_parent(
        &self,
        workspace: &Workspace,
        history: &[Message],
        model: &str,
    ) -> Option<String> {
        let transcript = render_parent_transcript(history);
        if transcript.trim().is_empty() {
            return None;
        }
        let request = AgentRequest {
            model: model.to_owned(),
            history: Vec::new(),
            user_input: transcript,
            system_prompt: Some(crate::SUMMARIZER_SYSTEM_PROMPT.to_owned()),
            mode: Some("compact".to_owned()),
            temperature: 0.0,
            max_tokens: Some(1000),
            limits: AgentLimits {
                max_iterations: 1,
                ..AgentLimits::default()
            },
            tool_names: Some(BTreeSet::new()),
            tool_context: ToolContext::new(workspace.clone(), self.merged_policy())
                .with_actor(crate::local_actor().id),
        };
        let sink = crate::PlanStepSink::default();
        let (_sender, signal) = CancelSignal::channel();
        let outcome = self
            .runtime
            .run(
                request,
                &sink,
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
                signal,
            )
            .await
            .ok()?;
        if !matches!(outcome, RunOutcome::Completed { .. }) {
            return None;
        }
        let text = mask_secrets(sink.text().trim());
        (!text.is_empty()).then_some(text)
    }
}
