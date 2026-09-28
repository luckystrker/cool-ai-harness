//! P1.8 `ask_user`: the question meta-tool rides the approval machinery with
//! `breakpointType: "question"`, surfaces the resolved answer, and fails
//! closed (`user_unavailable` / `question_timeout`) where no human can reply.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cool_agent::{
    ApprovalGate, ApprovalRequest, CancelSignal, EventSink, GateOutcome, RuntimeError, ToolContext,
    builtin_registry,
};
use cool_protocol::ApprovalOutcome;
use cool_security::{CapabilityPolicy, Decision, Workspace};
use serde_json::json;

struct ScriptedGate {
    requests: Mutex<Vec<ApprovalRequest>>,
    outcome: GateOutcome,
}

impl ScriptedGate {
    fn new(outcome: GateOutcome) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            outcome,
        }
    }
}

#[async_trait]
impl ApprovalGate for ScriptedGate {
    async fn request(
        &self,
        request: ApprovalRequest,
        _sink: &dyn EventSink,
        _cancel: &mut CancelSignal,
    ) -> Result<GateOutcome, RuntimeError> {
        self.requests.lock().unwrap().push(request);
        Ok(self.outcome.clone())
    }
}

/// Never resolves — used to exercise `timeout_secs`.
struct HangingGate;

#[async_trait]
impl ApprovalGate for HangingGate {
    async fn request(
        &self,
        _request: ApprovalRequest,
        _sink: &dyn EventSink,
        _cancel: &mut CancelSignal,
    ) -> Result<GateOutcome, RuntimeError> {
        std::future::pending::<()>().await;
        unreachable!()
    }
}

fn context() -> ToolContext {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(directory.path()).unwrap();
    std::mem::forget(directory);
    ToolContext::new(workspace, CapabilityPolicy::new(Some(Decision::Allow)))
}

fn ask_user() -> cool_agent::Tool {
    builtin_registry()
        .get("ask_user")
        .expect("ask_user is builtin")
}

#[tokio::test]
async fn ask_user_returns_the_resolved_answer() {
    let gate = Arc::new(ScriptedGate::new(GateOutcome {
        decision: ApprovalOutcome::Approved,
        answer: Some(json!("the green one")),
    }));
    let context = context().with_question_gate(gate.clone());

    let result = ask_user()
        .execute(
            &context,
            json!({
                "question": "Pick a color",
                "options": ["red", "green"],
                "allow_free_text": true,
            }),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "unexpected error: {result:?}");
    assert_eq!(result.output, json!({"answer": "the green one"}));

    let requests = gate.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.breakpoint_type.as_deref(), Some("question"));
    assert_eq!(request.call.name, "ask_user");
    assert_eq!(request.reason, "Pick a color");
    assert_eq!(
        request.call.arguments.get("options"),
        Some(&json!(["red", "green"]))
    );
}

#[tokio::test]
async fn ask_user_denied_fails_user_unavailable() {
    let gate = Arc::new(ScriptedGate::new(GateOutcome::decided(
        ApprovalOutcome::Denied,
    )));
    let context = context().with_question_gate(gate);

    let result = ask_user()
        .execute(&context, json!({"question": "Proceed?"}))
        .await
        .unwrap();

    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("user_unavailable"));
}

/// Subagents, one-shot CLI runs and scheduled runs install no question gate —
/// the tool fails closed instead of hanging forever.
#[tokio::test]
async fn ask_user_without_gate_fails_user_unavailable() {
    let result = ask_user()
        .execute(&context(), json!({"question": "Anyone there?"}))
        .await
        .unwrap();

    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("user_unavailable"));
}

#[tokio::test]
async fn ask_user_times_out() {
    let context = context().with_question_gate(Arc::new(HangingGate));

    let result = ask_user()
        .execute(
            &context,
            json!({"question": "Quick question", "timeout_secs": 0.05}),
        )
        .await
        .unwrap();

    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("question_timeout"));
}

#[tokio::test]
async fn ask_user_rejects_bad_arguments() {
    let tool = ask_user();
    // Missing `question`.
    assert!(
        tool.execute(&context(), json!({"options": ["a"]}))
            .await
            .is_err()
    );
    // `options` must be strings.
    assert!(
        tool.execute(&context(), json!({"question": "q", "options": [1]}))
            .await
            .is_err()
    );
    // Unknown argument.
    assert!(
        tool.execute(&context(), json!({"question": "q", "what": 1}))
            .await
            .is_err()
    );
}
