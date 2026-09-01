use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, CanonicalEvent, EventEnvelope, ItemEvent, PlanCreated,
    PlanProgress, PlanProgressStatus, PlanStep, RunStarted, RunTerminal, SessionCompacted,
    SubagentEvent, TextDelta, ToolApprovalRequired, ToolApprovalResolved, ToolCompleted,
    ToolFailed, ToolLifecycle, ToolRequested, UsageUpdated, V1Version,
};
use cool_security::{Decision, mask_json};
use cool_state::{BudgetDelta, DurableStore, StoreError};
use futures_util::{FutureExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use crate::context::{Message, MessageRole, ToolCall, compact_history, load_project_instructions};
use crate::provider::{ModelDriver, ModelEvent, ModelRequest, ProviderError, Usage};
use crate::tools::{ToolContext, ToolRegistry, ToolResult};

#[derive(Clone, Debug)]
pub struct AgentLimits {
    pub max_iterations: u32,
    pub max_total_tokens: Option<u64>,
    pub max_cost_micro_usd: Option<u64>,
    pub context_tokens: u64,
    pub max_provider_retries: u8,
    pub retry_backoff: Duration,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            max_total_tokens: None,
            max_cost_micro_usd: None,
            context_tokens: 96_000,
            max_provider_retries: 2,
            retry_backoff: Duration::from_millis(50),
        }
    }
}

#[derive(Clone)]
pub struct AgentRequest {
    pub model: String,
    pub history: Vec<Message>,
    pub user_input: String,
    pub system_prompt: Option<String>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    pub limits: AgentLimits,
    pub tool_names: Option<BTreeSet<String>>,
    pub tool_context: ToolContext,
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub call: ToolCall,
    pub reason: String,
}

#[derive(Clone)]
pub struct SubagentRequest {
    pub run_id: String,
    pub role: String,
    pub agent: AgentRequest,
    pub cancel: CancelSignal,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunOutcome {
    Completed {
        history: Vec<Message>,
        usage: Usage,
    },
    Cancelled {
        history: Vec<Message>,
        reason: String,
    },
    Failed {
        history: Vec<Message>,
        code: String,
    },
}

#[derive(Debug)]
pub enum RuntimeError {
    Provider(ProviderError),
    Store(StoreError),
    Sink(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(formatter, "provider error: {error}"),
            Self::Store(error) => write!(formatter, "state error: {error}"),
            Self::Sink(error) => write!(formatter, "event sink error: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<ProviderError> for RuntimeError {
    fn from(value: ProviderError) -> Self {
        Self::Provider(value)
    }
}

impl From<StoreError> for RuntimeError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError>;
    async fn load_history(&self) -> Result<Vec<Message>, RuntimeError> {
        Ok(Vec::new())
    }
    async fn reserve_usage(&self, _usage: &Usage) -> Result<(), RuntimeError> {
        Ok(())
    }
}

#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request(
        &self,
        request: ApprovalRequest,
        sink: &dyn EventSink,
        cancel: &mut CancelSignal,
    ) -> Result<ApprovalOutcome, RuntimeError>;
}

#[derive(Clone)]
pub struct AutoApprovalGate {
    pub outcome: ApprovalOutcome,
}

#[async_trait]
impl ApprovalGate for AutoApprovalGate {
    async fn request(
        &self,
        request: ApprovalRequest,
        sink: &dyn EventSink,
        _cancel: &mut CancelSignal,
    ) -> Result<ApprovalOutcome, RuntimeError> {
        sink.emit(CanonicalEvent::ToolApprovalRequired(ToolApprovalRequired {
            call_id: request.call.call_id.clone(),
            name: request.call.name.clone(),
            arguments: request.call.arguments.clone().into_iter().collect(),
            reason: request.reason,
            approval_id: request.approval_id.clone(),
            revision: 1,
            breakpoint_type: None,
            result_preview: None,
            current_content: None,
        }))
        .await?;
        sink.emit(CanonicalEvent::ToolApprovalResolved(ToolApprovalResolved {
            call_id: request.call.call_id,
            approval_id: request.approval_id,
            revision: 2,
            decision: self.outcome.clone(),
        }))
        .await?;
        Ok(self.outcome.clone())
    }
}

pub struct CancelSignal {
    receiver: watch::Receiver<Option<String>>,
}

impl Clone for CancelSignal {
    fn clone(&self) -> Self {
        Self {
            receiver: self.receiver.clone(),
        }
    }
}

impl CancelSignal {
    pub fn channel() -> (watch::Sender<Option<String>>, Self) {
        let (sender, receiver) = watch::channel(None);
        (sender, Self { receiver })
    }

    pub fn from_receiver(receiver: watch::Receiver<Option<String>>) -> Self {
        Self { receiver }
    }

    pub fn reason(&self) -> Option<String> {
        self.receiver.borrow().clone()
    }

    async fn changed(&mut self) -> Option<String> {
        if let Some(reason) = self.reason() {
            return Some(reason);
        }
        loop {
            if self.receiver.changed().await.is_err() {
                return std::future::pending().await;
            }
            if let Some(reason) = self.reason() {
                return Some(reason);
            }
        }
    }

    pub async fn wait(&mut self) -> String {
        self.changed()
            .await
            .unwrap_or_else(|| "cancelled".to_owned())
    }
}

#[derive(Clone)]
pub struct AgentRuntime {
    provider: Arc<dyn ModelDriver>,
    tools: ToolRegistry,
}

impl AgentRuntime {
    pub fn new(provider: Arc<dyn ModelDriver>, tools: ToolRegistry) -> Self {
        Self { provider, tools }
    }

    pub async fn run(
        &self,
        mut request: AgentRequest,
        sink: &dyn EventSink,
        approvals: &dyn ApprovalGate,
        mut cancel: CancelSignal,
    ) -> Result<RunOutcome, RuntimeError> {
        let mut history = if request.history.is_empty() {
            sink.load_history().await?
        } else {
            request.history
        };
        if let Some(system_prompt) = request.system_prompt.take()
            && !history
                .iter()
                .any(|message| message.role == MessageRole::System)
        {
            history.insert(0, Message::text(MessageRole::System, system_prompt));
        }
        if let Ok(Some(instructions)) = load_project_instructions(&request.tool_context.workspace) {
            if let Some(system) = history
                .iter_mut()
                .find(|message| message.role == MessageRole::System)
            {
                let content = system.content.get_or_insert_default();
                content.push_str("\n\n");
                content.push_str(&instructions);
            } else {
                history.insert(0, Message::text(MessageRole::System, instructions));
            }
        }
        sink.emit(CanonicalEvent::RunStarted(RunStarted {
            model: Some(request.model.clone()),
            mode: Some("m7_rust_agent".to_owned()),
        }))
        .await?;
        let user_message = Message::text(MessageRole::User, request.user_input);
        sink.emit(CanonicalEvent::ItemCompleted(ItemEvent {
            role: Some("user".to_owned()),
            content: user_message.content.clone(),
            tool_calls: Vec::new(),
        }))
        .await?;
        history.push(user_message);
        let mut total_usage = Usage::default();
        let mut cost_complete = true;
        if request.limits.max_iterations == 0 {
            return finish_failed(sink, history, "iteration_limit".to_owned()).await;
        }
        for iteration in 1..=request.limits.max_iterations {
            let mut usage_observed = false;
            if let Some(reason) = cancel.reason() {
                return finish_cancelled(sink, history, reason).await;
            }
            let compacted = compact_history(&history, request.limits.context_tokens);
            if compacted.dropped_messages > 0 {
                sink.emit(CanonicalEvent::SessionCompacted(SessionCompacted {
                    retained_items: compacted.messages.len() as u32,
                    summary_item_id: None,
                }))
                .await?;
            }
            let definitions = self
                .tools
                .definitions()
                .into_iter()
                .filter(|definition| {
                    request
                        .tool_names
                        .as_ref()
                        .is_none_or(|names| names.contains(&definition.name))
                })
                .collect();
            let model_request = ModelRequest {
                model: request.model.clone(),
                messages: compacted.messages,
                tools: definitions,
                temperature: request.temperature,
                max_tokens: request.max_tokens,
            };
            let mut attempt = 0_u8;
            let mut stream = loop {
                match self.provider.stream(model_request.clone()).await {
                    Ok(stream) => break stream,
                    Err(error)
                        if error.retryable && attempt < request.limits.max_provider_retries =>
                    {
                        attempt += 1;
                        tokio::select! {
                            () = sleep(request.limits.retry_backoff.saturating_mul(u32::from(attempt))) => {}
                            reason = cancel.changed() => {
                                return finish_cancelled(sink, history, reason.unwrap_or_else(|| "cancelled".to_owned())).await;
                            }
                        }
                    }
                    Err(error) => return finish_failed(sink, history, error.code).await,
                }
            };
            let mut content = String::new();
            let mut calls = Vec::new();
            let mut progressed = false;
            loop {
                let next = tokio::select! {
                    event = stream.next() => event,
                    reason = cancel.changed() => {
                        return finish_cancelled(sink, history, reason.unwrap_or_else(|| "cancelled".to_owned())).await;
                    }
                };
                let Some(event) = next else { break };
                match event {
                    Ok(ModelEvent::Content(text)) => {
                        progressed = true;
                        content.push_str(&text);
                        sink.emit(CanonicalEvent::ContentDelta(TextDelta {
                            text,
                            channel: Some("final".to_owned()),
                        }))
                        .await?;
                    }
                    Ok(ModelEvent::Reasoning(text)) => {
                        progressed = true;
                        sink.emit(CanonicalEvent::ReasoningDelta(TextDelta {
                            text,
                            channel: Some("analysis".to_owned()),
                        }))
                        .await?;
                    }
                    Ok(ModelEvent::ToolCall(call)) => {
                        progressed = true;
                        calls.push(call);
                    }
                    Ok(ModelEvent::Usage(usage)) => {
                        usage_observed = true;
                        cost_complete &= usage.cost_micro_usd.is_some();
                        if sink.reserve_usage(&usage).await.is_err() {
                            return finish_failed(sink, history, "run_budget_exceeded".to_owned())
                                .await;
                        }
                        add_usage(&mut total_usage, &usage);
                        if !cost_complete {
                            total_usage.cost_micro_usd = None;
                        }
                        sink.emit(CanonicalEvent::UsageUpdated(UsageUpdated {
                            prompt_tokens: usage.prompt_tokens,
                            completion_tokens: usage.completion_tokens,
                            total_tokens: usage.total_tokens,
                            cost_usd: usage.cost_micro_usd.map(|cost| cost as f64 / 1_000_000.0),
                        }))
                        .await?;
                    }
                    Ok(ModelEvent::Finish { .. }) => break,
                    Err(error)
                        if error.retryable
                            && !progressed
                            && attempt < request.limits.max_provider_retries =>
                    {
                        attempt += 1;
                        tokio::select! {
                            () = sleep(request.limits.retry_backoff.saturating_mul(u32::from(attempt))) => {}
                            reason = cancel.changed() => {
                                return finish_cancelled(sink, history, reason.unwrap_or_else(|| "cancelled".to_owned())).await;
                            }
                        }
                        match self.provider.stream(model_request.clone()).await {
                            Ok(replacement) => stream = replacement,
                            Err(next_error) => {
                                return finish_failed(sink, history, next_error.code).await;
                            }
                        }
                    }
                    Err(error) => return finish_failed(sink, history, error.code).await,
                }
            }
            if (!usage_observed
                && (request.limits.max_total_tokens.is_some()
                    || request.limits.max_cost_micro_usd.is_some()))
                || limits_exceeded(&request.limits, &total_usage)
            {
                return finish_failed(sink, history, "run_budget_exceeded".to_owned()).await;
            }
            let assistant_message = Message {
                role: MessageRole::Assistant,
                content: (!content.is_empty()).then_some(content),
                tool_calls: calls.clone(),
                tool_call_id: None,
                name: None,
            };
            sink.emit(CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("assistant".to_owned()),
                content: assistant_message.content.clone(),
                tool_calls: assistant_message
                    .tool_calls
                    .iter()
                    .map(|call| ToolRequested {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone().into_iter().collect(),
                    })
                    .collect(),
            }))
            .await?;
            history.push(assistant_message);
            if calls.is_empty() {
                sink.emit(CanonicalEvent::RunCompleted(RunTerminal {
                    reason: "stop".to_owned(),
                    error_code: None,
                }))
                .await?;
                return Ok(RunOutcome::Completed {
                    history,
                    usage: total_usage,
                });
            }
            let batch = self
                .execute_tool_batch(calls, &request.tool_context, sink, approvals, &mut cancel)
                .await?;
            for (call, result) in batch.results {
                let message = Message::tool_result(
                    &call,
                    serde_json::to_string(&result.output).unwrap_or_else(|_| "null".to_owned()),
                );
                history.push(message);
            }
            if let Some(reason) = batch.cancelled {
                return finish_cancelled(sink, history, reason).await;
            }
            if iteration == request.limits.max_iterations {
                return finish_failed(sink, history, "iteration_limit".to_owned()).await;
            }
        }
        unreachable!("positive iteration limit exits through the loop")
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_batch(
        &self,
        calls: Vec<ToolCall>,
        context: &ToolContext,
        sink: &dyn EventSink,
        approvals: &dyn ApprovalGate,
        cancel: &mut CancelSignal,
    ) -> Result<ToolBatchOutcome, RuntimeError> {
        let mut immediate = HashMap::new();
        let mut runnable = Vec::new();
        for (index, call) in calls.iter().cloned().enumerate() {
            sink.emit(CanonicalEvent::ToolRequested(ToolRequested {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone().into_iter().collect(),
            }))
            .await?;
            let Some(tool) = self.tools.get(&call.name) else {
                let result = ToolResult::error("tool_not_found", "tool is not registered");
                emit_tool_result(sink, &call, &result).await?;
                immediate.insert(index, (call, result));
                continue;
            };
            let decision = context
                .policy
                .evaluate(tool.capabilities.iter().copied(), tool.default_decision)
                .effective;
            if decision == Decision::Deny {
                let result = ToolResult::error("capability_denied", "tool capability was denied");
                emit_tool_result(sink, &call, &result).await?;
                immediate.insert(index, (call, result));
                continue;
            }
            if decision == Decision::Ask {
                let outcome = approvals
                    .request(
                        ApprovalRequest {
                            approval_id: format!("approval-{}", Uuid::new_v4()),
                            call: call.clone(),
                            reason: "tool requires approval".to_owned(),
                        },
                        sink,
                        cancel,
                    )
                    .await?;
                if outcome != ApprovalOutcome::Approved {
                    let result = ToolResult::error("approval_denied", "tool approval was denied");
                    emit_tool_result(sink, &call, &result).await?;
                    immediate.insert(index, (call, result));
                    continue;
                }
            }
            runnable.push((index, call, tool));
        }
        let mut join_set = JoinSet::new();
        for (index, call, tool) in runnable {
            sink.emit(CanonicalEvent::ToolStarted(ToolLifecycle {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
            }))
            .await?;
            let mut context = context.clone();
            context.cancel = Some(cancel.clone());
            join_set.spawn(async move {
                let arguments = Value::Object(call.arguments.clone());
                let result = std::panic::AssertUnwindSafe(tool.execute(&context, arguments))
                    .catch_unwind()
                    .await
                    .map_or_else(
                        |_| ToolResult::error("tool_panicked", "tool panicked inside runtime"),
                        |result| {
                            result.map_or_else(
                                |error| ToolResult::error("tool_runtime_error", error.to_string()),
                                ToolResult::masked,
                            )
                        },
                    );
                (index, call, result)
            });
        }
        let mut completed = immediate;
        let mut pending = calls
            .iter()
            .enumerate()
            .filter(|(index, _)| !completed.contains_key(index))
            .map(|(index, call)| (index, call.clone()))
            .collect::<HashMap<_, _>>();
        let mut cancelled = None;
        while !join_set.is_empty() {
            tokio::select! {
                joined = join_set.join_next() => {
                    let Some(Ok((index, call, result))) = joined else { continue };
                    pending.remove(&index);
                    emit_tool_result(sink, &call, &result).await?;
                    emit_plan_events(sink, &call, &result).await?;
                    completed.insert(index, (call, result));
                }
                reason = cancel.changed() => {
                    cancelled = Some(reason.unwrap_or_else(|| "cancelled".to_owned()));
                    let grace = async {
                        while let Some(joined) = join_set.join_next().await {
                            if let Ok((index, call, result)) = joined {
                                pending.remove(&index);
                                emit_tool_result(sink, &call, &result).await?;
                                completed.insert(index, (call, result));
                            }
                        }
                        Ok::<(), RuntimeError>(())
                    };
                    match timeout(Duration::from_secs(1), grace).await {
                        Ok(result) => result?,
                        Err(_) => {
                            join_set.abort_all();
                            while join_set.join_next().await.is_some() {}
                        }
                    }
                    break;
                }
            }
        }
        if cancelled.is_some() {
            for (index, call) in pending {
                let result = ToolResult::error("tool_batch_cancelled", "tool batch was cancelled");
                emit_tool_result(sink, &call, &result).await?;
                completed.insert(index, (call, result));
            }
        }
        let mut results = completed.into_iter().collect::<Vec<_>>();
        results.sort_by_key(|(index, _)| *index);
        Ok(ToolBatchOutcome {
            results: results.into_iter().map(|(_, result)| result).collect(),
            cancelled,
        })
    }

    pub async fn run_subagent(
        &self,
        request: SubagentRequest,
        parent_sink: &dyn EventSink,
        child_sink: &dyn EventSink,
        approvals: &dyn ApprovalGate,
    ) -> Result<RunOutcome, RuntimeError> {
        parent_sink
            .emit(CanonicalEvent::SubagentStarted(SubagentEvent {
                subagent_run_id: request.run_id.clone(),
                name: Some(request.role.clone()),
                status: "running".to_owned(),
                summary: None,
                error: None,
            }))
            .await?;
        let outcome = self
            .run(request.agent, child_sink, approvals, request.cancel)
            .await;
        let (event, result) = match outcome {
            Ok(outcome @ RunOutcome::Completed { .. }) => (
                CanonicalEvent::SubagentCompleted(SubagentEvent {
                    subagent_run_id: request.run_id.clone(),
                    name: Some(request.role.clone()),
                    status: "completed".to_owned(),
                    summary: Some("completed".to_owned()),
                    error: None,
                }),
                Ok(outcome),
            ),
            Ok(outcome) => (
                CanonicalEvent::SubagentFailed(SubagentEvent {
                    subagent_run_id: request.run_id.clone(),
                    name: Some(request.role.clone()),
                    status: "failed".to_owned(),
                    summary: Some("subagent did not complete".to_owned()),
                    error: Some("subagent_not_completed".to_owned()),
                }),
                Ok(outcome),
            ),
            Err(error) => (
                CanonicalEvent::SubagentFailed(SubagentEvent {
                    subagent_run_id: request.run_id,
                    name: Some(request.role),
                    status: "failed".to_owned(),
                    summary: Some(error.to_string()),
                    error: Some("subagent_runtime_error".to_owned()),
                }),
                Err(error),
            ),
        };
        parent_sink.emit(event).await?;
        result
    }
}

struct ToolBatchOutcome {
    results: Vec<(ToolCall, ToolResult)>,
    cancelled: Option<String>,
}

async fn emit_tool_result(
    sink: &dyn EventSink,
    call: &ToolCall,
    result: &ToolResult,
) -> Result<(), RuntimeError> {
    if result.is_error {
        sink.emit(CanonicalEvent::ToolFailed(ToolFailed {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            error_code: result
                .error_code
                .clone()
                .unwrap_or_else(|| "tool_failed".to_owned()),
            message: result
                .output
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }))
        .await?;
    } else {
        sink.emit(CanonicalEvent::ToolCompleted(ToolCompleted {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            result: result.output.clone(),
        }))
        .await?;
    }
    Ok(())
}

async fn emit_plan_events(
    sink: &dyn EventSink,
    call: &ToolCall,
    result: &ToolResult,
) -> Result<(), RuntimeError> {
    if call.name != "update_plan" || result.is_error {
        return Ok(());
    }
    let Some(plan_id) = result.output.get("planId").and_then(Value::as_str) else {
        return Ok(());
    };
    let steps = result.output["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    sink.emit(CanonicalEvent::PlanCreated(PlanCreated {
        plan_id: plan_id.to_owned(),
        title: result.output["title"].as_str().map(str::to_owned),
        total_steps: steps.len() as u32,
    }))
    .await?;
    let mut completed = 0_u32;
    for (position, step) in steps.iter().enumerate() {
        let status = step["status"].as_str().unwrap_or("pending").to_owned();
        if status == "completed" {
            completed += 1;
        }
        let step_event = PlanStep {
            plan_id: plan_id.to_owned(),
            position: position as u32,
            title: step["title"].as_str().unwrap_or("step").to_owned(),
            status: status.clone(),
            result_summary: None,
        };
        match status.as_str() {
            "in_progress" => {
                sink.emit(CanonicalEvent::PlanStepStarted(step_event))
                    .await?;
            }
            "completed" | "failed" => {
                sink.emit(CanonicalEvent::PlanStepCompleted(step_event))
                    .await?;
            }
            _ => {}
        }
    }
    sink.emit(CanonicalEvent::PlanProgress(PlanProgress {
        plan_id: plan_id.to_owned(),
        completed_steps: completed,
        total_steps: steps.len() as u32,
        message: None,
        status: if completed == steps.len() as u32 {
            PlanProgressStatus::Completed
        } else {
            PlanProgressStatus::Executing
        },
    }))
    .await?;
    Ok(())
}

async fn finish_cancelled(
    sink: &dyn EventSink,
    history: Vec<Message>,
    reason: String,
) -> Result<RunOutcome, RuntimeError> {
    sink.emit(CanonicalEvent::RunCancelled(RunTerminal {
        reason: reason.clone(),
        error_code: None,
    }))
    .await?;
    Ok(RunOutcome::Cancelled { history, reason })
}

async fn finish_failed(
    sink: &dyn EventSink,
    history: Vec<Message>,
    code: String,
) -> Result<RunOutcome, RuntimeError> {
    sink.emit(CanonicalEvent::RunFailed(RunTerminal {
        reason: code.clone(),
        error_code: Some(code.clone()),
    }))
    .await?;
    Ok(RunOutcome::Failed { history, code })
}

fn add_usage(total: &mut Usage, addition: &Usage) {
    total.prompt_tokens = total.prompt_tokens.saturating_add(addition.prompt_tokens);
    total.completion_tokens = total
        .completion_tokens
        .saturating_add(addition.completion_tokens);
    total.total_tokens = total.total_tokens.saturating_add(addition.total_tokens);
    total.cost_micro_usd = match (total.cost_micro_usd, addition.cost_micro_usd) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (None, value) | (value, None) => value,
    };
}

fn limits_exceeded(limits: &AgentLimits, usage: &Usage) -> bool {
    limits
        .max_total_tokens
        .is_some_and(|limit| usage.total_tokens > limit)
        || limits
            .max_cost_micro_usd
            .is_some_and(|limit| usage.cost_micro_usd.is_none_or(|actual| actual > limit))
}

#[derive(Clone)]
pub struct StoreEventSink {
    store: DurableStore,
    owner_actor_id: String,
    session_id: String,
    run_id: String,
    actor: ActorRef,
    source: String,
    budget_window: Option<String>,
}

impl StoreEventSink {
    pub fn new(
        store: DurableStore,
        owner_actor_id: impl Into<String>,
        session_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            store,
            owner_actor_id: owner_actor_id.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            actor: ActorRef {
                id: "cool-agent".to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-agent-m7".to_owned(),
            budget_window: None,
        }
    }

    pub fn with_budget_window(mut self, window: impl Into<String>) -> Self {
        self.budget_window = Some(window.into());
        self
    }
}

#[async_trait]
impl EventSink for StoreEventSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        let event = mask_canonical_event(event)?;
        if matches!(event, CanonicalEvent::RunCancelled(_)) {
            let run = self.store.run(&self.run_id, &self.owner_actor_id)?;
            if run.status == cool_state::RunStatus::Cancelled
                && let Some(existing) = self
                    .store
                    .all_events(&self.run_id, &self.owner_actor_id)?
                    .into_iter()
                    .last()
                    .filter(|event| matches!(event.event, CanonicalEvent::RunCancelled(_)))
            {
                return Ok(existing);
            }
        }
        if let CanonicalEvent::ToolFailed(requested) = &event {
            let run = self.store.run(&self.run_id, &self.owner_actor_id)?;
            if run.status.is_terminal()
                && let Some(existing) = self
                    .store
                    .all_events(&self.run_id, &self.owner_actor_id)?
                    .into_iter()
                    .find(|event| {
                        matches!(
                            &event.event,
                            CanonicalEvent::ToolFailed(stored)
                                if stored.call_id == requested.call_id
                        )
                    })
            {
                return Ok(existing);
            }
        }
        let envelope = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: timestamp(),
            actor: self.actor.clone(),
            source: self.source.clone(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: BTreeMap::new(),
        };
        Ok(self
            .store
            .append_event_auto(&self.owner_actor_id, envelope)?)
    }

    async fn reserve_usage(&self, usage: &Usage) -> Result<(), RuntimeError> {
        if let Some(window) = &self.budget_window {
            self.store.reserve_budget(
                &self.owner_actor_id,
                window,
                BudgetDelta {
                    tokens: usage.total_tokens,
                    cost_microusd: usage.cost_micro_usd,
                    iterations: 1,
                    proactive_actions: 0,
                },
            )?;
        }
        Ok(())
    }

    async fn load_history(&self) -> Result<Vec<Message>, RuntimeError> {
        history_from_events(
            &self
                .store
                .session_events(&self.session_id, &self.owner_actor_id)?,
        )
    }
}

pub fn mask_canonical_event(event: CanonicalEvent) -> Result<CanonicalEvent, RuntimeError> {
    let mut value = serde_json::to_value(event)
        .map_err(|error| RuntimeError::Sink(format!("event masking failed: {error}")))?;
    mask_json(&mut value);
    serde_json::from_value(value)
        .map_err(|error| RuntimeError::Sink(format!("masked event is invalid: {error}")))
}

pub fn history_from_events(events: &[EventEnvelope]) -> Result<Vec<Message>, RuntimeError> {
    let mut history = Vec::new();
    for envelope in events {
        match &envelope.event {
            CanonicalEvent::ItemCompleted(item)
                if matches!(item.role.as_deref(), Some("user" | "assistant")) =>
            {
                let role = if item.role.as_deref() == Some("user") {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                };
                history.push(Message {
                    role,
                    content: item.content.clone(),
                    tool_calls: item
                        .tool_calls
                        .iter()
                        .map(|call| ToolCall {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone().into_iter().collect(),
                        })
                        .collect(),
                    tool_call_id: None,
                    name: None,
                });
            }
            CanonicalEvent::ToolCompleted(tool) => history.push(Message {
                role: MessageRole::Tool,
                content: Some(
                    serde_json::to_string(&tool.result)
                        .map_err(|error| RuntimeError::Sink(error.to_string()))?,
                ),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool.call_id.clone()),
                name: Some(tool.name.clone()),
            }),
            CanonicalEvent::ToolFailed(tool) => history.push(Message {
                role: MessageRole::Tool,
                content: Some(
                    serde_json::to_string(&json!({
                        "error": tool.message,
                        "errorCode": tool.error_code,
                    }))
                    .map_err(|error| RuntimeError::Sink(error.to_string()))?,
                ),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool.call_id.clone()),
                name: Some(tool.name.clone()),
            }),
            _ => {}
        }
    }
    Ok(history)
}

fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("unix-ms:{millis}")
}
