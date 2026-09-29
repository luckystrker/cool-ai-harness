//! M11 canonical plan execution coverage: `plan.created` persists a durable
//! draft plan and `plans.execute` runs the approved steps through the Rust
//! runtime, emitting canonical `plan.*` events on a durable run.

use std::sync::Arc;
use std::time::Duration;

use cool_agent::{AgentRuntime, ModelEvent, ScriptedDriver, ToolCall, builtin_registry};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::*;
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::domains::conversations::NewConversation;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::sleep;

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("valid key")
}

struct Harness {
    client: AppClient,
    store: Arc<LegacyStore>,
    _task: tokio::task::JoinHandle<std::io::Result<()>>,
    _directory: tempfile::TempDir,
}

async fn harness(driver: Arc<ScriptedDriver>) -> Harness {
    let directory = tempdir().expect("tempdir");
    let store = Arc::new(LegacyStore::in_memory().expect("legacy store"));
    store.ensure_actor("local-user").expect("actor");
    let config = ServerConfig {
        legacy_store: Some(store.clone()),
        event_delay: Duration::ZERO,
        ..ServerConfig::default()
    };
    let server = AppServer::with_agent_runtime(
        config,
        DurableStore::in_memory().expect("durable"),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(directory.path()).expect("workspace"),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .expect("server");
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client.initialize("plan-tests", "1").await.expect("init");
    Harness {
        client,
        store,
        _task: task,
        _directory: directory,
    }
}

async fn link(client: &AppClient, conversation_id: i64) -> String {
    let linked = client
        .request(Command::SessionForConversation(
            SessionForConversationParams {
                idempotency_key: key(&format!("link-{conversation_id}")),
                conversation_id,
                session_id: None,
            },
        ))
        .await
        .expect("link");
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };
    link.session_id
}

async fn run_events(client: &AppClient, run_id: &str) -> Vec<EventEnvelope> {
    let events = client
        .request(Command::RunEvents(RunEventsParams {
            run_id: run_id.to_owned(),
            after_seq: None,
            limit: 200,
        }))
        .await
        .expect("run events");
    let ResponsePayload::EventPage(page) = events else {
        panic!("unexpected payload");
    };
    page.events
}

async fn wait_run_terminal(client: &AppClient, run_id: &str) -> Vec<EventEnvelope> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = run_events(client, run_id).await;
        if events.iter().any(|envelope| {
            matches!(
                envelope.event,
                CanonicalEvent::RunCompleted(_)
                    | CanonicalEvent::RunFailed(_)
                    | CanonicalEvent::RunCancelled(_)
            )
        }) {
            return events;
        }
        assert!(std::time::Instant::now() < deadline, "run did not finish");
        sleep(Duration::from_millis(20)).await;
    }
}

fn update_plan_script() -> ScriptedDriver {
    ScriptedDriver::new([
        Ok(vec![ModelEvent::ToolCall(ToolCall {
            call_id: "call-1".to_owned(),
            name: "update_plan".to_owned(),
            arguments: serde_json::Map::from_iter([
                ("planId".to_owned(), json!("model-plan-1")),
                ("title".to_owned(), json!("Migrate")),
                (
                    "steps".to_owned(),
                    json!([
                        {"title": "first", "status": "pending"},
                        {"title": "second", "status": "pending"}
                    ]),
                ),
            ]),
        })]),
        Ok(vec![
            ModelEvent::Content("planned".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ])
}

#[tokio::test]
async fn plan_created_persists_a_durable_plan_with_a_store_id() {
    let harness = harness(Arc::new(update_plan_script())).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    let session_id = link(&harness.client, parent).await;
    let accepted = harness
        .client
        .request(Command::SessionPrompt(SessionPromptParams {
            idempotency_key: key("plan-turn"),
            session_id,
            content: vec![ContentPart::Text {
                text: "make a plan".to_owned(),
            }],
            model: None,
            system_prompt: None,
            plan_mode: true,
            long_task_mode: false,
        }))
        .await
        .expect("prompt");
    let ResponsePayload::PromptAccepted(accepted) = accepted else {
        panic!("unexpected payload");
    };
    let events = wait_run_terminal(&harness.client, &accepted.run_id).await;
    let plan_created = events
        .iter()
        .find_map(|envelope| match &envelope.event {
            CanonicalEvent::PlanCreated(plan) => Some(plan.clone()),
            _ => None,
        })
        .expect("plan.created event");
    assert_eq!(plan_created.total_steps, 2);
    let store_plan_id = plan_created
        .store_plan_id
        .expect("the app server persisted a durable plan");
    let plan = harness
        .store
        .get_plan("local-user", parent, store_plan_id)
        .expect("durable plan");
    assert_eq!(plan.title.as_deref(), Some("Migrate"));
    assert_eq!(plan.status, "draft");
    assert_eq!(
        harness.store.list_plan_steps(plan.id).expect("steps").len(),
        2
    );
}

#[tokio::test]
async fn plan_execute_runs_steps_and_emits_canonical_events() {
    let harness = harness(Arc::new(ScriptedDriver::echo())).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    link(&harness.client, parent).await;
    let plan = harness
        .store
        .create_plan(
            "local-user",
            parent,
            None,
            Some("Two steps"),
            &json!([
                {"position": 0, "title": "first"},
                {"position": 1, "title": "second"}
            ]),
        )
        .expect("plan");
    harness
        .client
        .request(Command::PlansApprove(PlanApproveParams {
            idempotency_key: key("approve"),
            conversation_id: parent,
            plan_id: plan.id,
            approved: true,
        }))
        .await
        .expect("approve");

    let executed = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("execute");
    let ResponsePayload::PlansExecuted(result) = executed else {
        panic!("unexpected payload");
    };
    assert_eq!(result.plan_id, plan.id);
    assert_eq!(result.status, "running");

    let events = wait_run_terminal(&harness.client, &result.run_id).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e.event, CanonicalEvent::PlanCreated(_))),
        "plan.created"
    );
    let started = events
        .iter()
        .filter(|e| matches!(e.event, CanonicalEvent::PlanStepStarted(_)))
        .count();
    let completed = events
        .iter()
        .filter(|e| matches!(e.event, CanonicalEvent::PlanStepCompleted(_)))
        .count();
    assert_eq!(started, 2);
    assert_eq!(completed, 2);
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(CanonicalEvent::RunCompleted(_))
    ));

    let stored = harness
        .store
        .get_plan("local-user", parent, plan.id)
        .expect("plan");
    assert_eq!(stored.status, "completed");
    for step in harness.store.list_plan_steps(plan.id).expect("steps") {
        assert_eq!(step.status, "completed");
        assert!(step.result_summary.is_some());
    }
}

#[tokio::test]
async fn plan_execute_failure_breaks_and_skips_unmet_dependencies() {
    let harness = harness(Arc::new(ScriptedDriver::echo())).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    link(&harness.client, parent).await;
    // Step 0 delegates to a role that does not exist (fails); step 1 depends on
    // step 0 (dependencies unmet -> skipped).
    let plan = harness
        .store
        .create_plan(
            "local-user",
            parent,
            None,
            Some("Failing"),
            &json!([
                {"position": 0, "title": "boom", "delegate_role": "missing-role"},
                {"position": 1, "title": "later", "depends_on": [0]}
            ]),
        )
        .expect("plan");
    harness
        .client
        .request(Command::PlansApprove(PlanApproveParams {
            idempotency_key: key("approve-fail"),
            conversation_id: parent,
            plan_id: plan.id,
            approved: true,
        }))
        .await
        .expect("approve");
    let executed = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute-fail"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("execute");
    let ResponsePayload::PlansExecuted(result) = executed else {
        panic!("unexpected payload");
    };
    let events = wait_run_terminal(&harness.client, &result.run_id).await;
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(CanonicalEvent::RunFailed(_))
    ));
    let stored = harness
        .store
        .get_plan("local-user", parent, plan.id)
        .expect("plan");
    assert_eq!(stored.status, "failed");
    // The loop breaks on failure, so the later step stays pending.
    let later = harness
        .store
        .list_plan_steps(plan.id)
        .expect("steps")
        .into_iter()
        .find(|step| step.position == 1)
        .expect("second step");
    assert_eq!(later.status, "pending");
}

#[tokio::test]
async fn plan_execute_replay_is_idempotent_and_rejects_a_second_run() {
    let harness = harness(Arc::new(ScriptedDriver::echo())).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    link(&harness.client, parent).await;
    let plan = harness
        .store
        .create_plan(
            "local-user",
            parent,
            None,
            Some("Once"),
            &json!([{"position": 0, "title": "only"}]),
        )
        .expect("plan");
    harness
        .client
        .request(Command::PlansApprove(PlanApproveParams {
            idempotency_key: key("approve-once"),
            conversation_id: parent,
            plan_id: plan.id,
            approved: true,
        }))
        .await
        .expect("approve");
    let first = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute-once"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("execute");
    let ResponsePayload::PlansExecuted(first) = first else {
        panic!("unexpected payload");
    };
    let replay = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute-once"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("replay");
    let ResponsePayload::PlansExecuted(replay) = replay else {
        panic!("unexpected payload");
    };
    assert_eq!(first.run_id, replay.run_id);

    wait_run_terminal(&harness.client, &first.run_id).await;
    // The plan is now completed: a different key must not start another run.
    let error = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute-again"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect_err("a second execution must be rejected");
    assert!(matches!(
        error,
        cool_app_server::ClientError::Protocol(protocol) if protocol.cool_code == "invalid_input"
    ));
}

#[tokio::test]
async fn plan_cancel_during_execution_cancels_the_run() {
    let harness = harness(Arc::new(ScriptedDriver::echo_with_delay(
        Duration::from_millis(600),
    )))
    .await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    link(&harness.client, parent).await;
    let plan = harness
        .store
        .create_plan(
            "local-user",
            parent,
            None,
            Some("Cancel me"),
            &json!([
                {"position": 0, "title": "slow"},
                {"position": 1, "title": "next"}
            ]),
        )
        .expect("plan");
    harness
        .client
        .request(Command::PlansApprove(PlanApproveParams {
            idempotency_key: key("approve-cancel"),
            conversation_id: parent,
            plan_id: plan.id,
            approved: true,
        }))
        .await
        .expect("approve");
    let executed = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute-cancel"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("execute");
    let ResponsePayload::PlansExecuted(result) = executed else {
        panic!("unexpected payload");
    };
    sleep(Duration::from_millis(100)).await;
    harness
        .client
        .request(Command::PlansCancel(IdempotentPlanIdParams {
            idempotency_key: key("cancel-plan"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("cancel");
    let events = wait_run_terminal(&harness.client, &result.run_id).await;
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(CanonicalEvent::RunCancelled(_))
    ));
    assert_eq!(
        harness
            .store
            .get_plan("local-user", parent, plan.id)
            .expect("plan")
            .status,
        "cancelled"
    );
    // The boundary check must stop the loop: the second step never runs.
    let later = harness
        .store
        .list_plan_steps(plan.id)
        .expect("steps")
        .into_iter()
        .find(|step| step.position == 1)
        .expect("second step");
    assert_eq!(later.status, "pending");
}

#[tokio::test]
async fn plan_created_without_a_linked_conversation_has_no_store_id() {
    let harness = harness(Arc::new(update_plan_script())).await;
    let created = harness
        .client
        .request(Command::SessionCreate(SessionCreateParams {
            idempotency_key: key("session"),
            title: Some("Standalone".to_owned()),
            project_key: None,
        }))
        .await
        .expect("session");
    let ResponsePayload::SessionCreated(created) = created else {
        panic!("unexpected payload");
    };
    let accepted = harness
        .client
        .request(Command::SessionPrompt(SessionPromptParams {
            idempotency_key: key("prompt"),
            session_id: created.session_id,
            content: vec![ContentPart::Text {
                text: "plan".to_owned(),
            }],
            model: None,
            system_prompt: None,
            plan_mode: true,
            long_task_mode: false,
        }))
        .await
        .expect("prompt");
    let ResponsePayload::PromptAccepted(accepted) = accepted else {
        panic!("unexpected payload");
    };
    let events = wait_run_terminal(&harness.client, &accepted.run_id).await;
    let plan = events
        .iter()
        .find_map(|envelope| match &envelope.event {
            CanonicalEvent::PlanCreated(plan) => Some(plan.clone()),
            _ => None,
        })
        .expect("plan.created");
    assert!(plan.store_plan_id.is_none());
}

#[tokio::test]
async fn plan_dependency_cycle_skips_the_dependent_step() {
    let harness = harness(Arc::new(ScriptedDriver::echo())).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    link(&harness.client, parent).await;
    // A two-node cycle is appended in position order; step 0's dependency is
    // still pending, so it is skipped, which unblocks step 1.
    let plan = harness
        .store
        .create_plan(
            "local-user",
            parent,
            None,
            Some("Cycle"),
            &json!([
                {"position": 0, "title": "zero", "depends_on": [1]},
                {"position": 1, "title": "one", "depends_on": [0]}
            ]),
        )
        .expect("plan");
    harness
        .client
        .request(Command::PlansApprove(PlanApproveParams {
            idempotency_key: key("approve-cycle"),
            conversation_id: parent,
            plan_id: plan.id,
            approved: true,
        }))
        .await
        .expect("approve");
    let executed = harness
        .client
        .request(Command::PlansExecute(IdempotentPlanIdParams {
            idempotency_key: key("execute-cycle"),
            conversation_id: parent,
            plan_id: plan.id,
        }))
        .await
        .expect("execute");
    let ResponsePayload::PlansExecuted(result) = executed else {
        panic!("unexpected payload");
    };
    wait_run_terminal(&harness.client, &result.run_id).await;
    let steps = harness.store.list_plan_steps(plan.id).expect("steps");
    let zero = steps.iter().find(|step| step.position == 0).expect("zero");
    let one = steps.iter().find(|step| step.position == 1).expect("one");
    assert_eq!(zero.status, "skipped");
    assert_eq!(one.status, "completed");
}

#[tokio::test]
async fn repeated_update_plan_persists_one_draft() {
    let driver = Arc::new(ScriptedDriver::new([
        Ok(vec![ModelEvent::ToolCall(ToolCall {
            call_id: "c1".to_owned(),
            name: "update_plan".to_owned(),
            arguments: serde_json::Map::from_iter([
                ("planId".to_owned(), json!("model-plan-1")),
                (
                    "steps".to_owned(),
                    json!([{"title": "first", "status": "pending"}]),
                ),
            ]),
        })]),
        Ok(vec![ModelEvent::ToolCall(ToolCall {
            call_id: "c2".to_owned(),
            name: "update_plan".to_owned(),
            arguments: serde_json::Map::from_iter([
                ("planId".to_owned(), json!("model-plan-1")),
                (
                    "steps".to_owned(),
                    json!([{"title": "first", "status": "pending"}]),
                ),
            ]),
        })]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]));
    let harness = harness(driver).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    let session_id = link(&harness.client, parent).await;
    let accepted = harness
        .client
        .request(Command::SessionPrompt(SessionPromptParams {
            idempotency_key: key("two-plans"),
            session_id,
            content: vec![ContentPart::Text {
                text: "plan twice".to_owned(),
            }],
            model: None,
            system_prompt: None,
            plan_mode: true,
            long_task_mode: false,
        }))
        .await
        .expect("prompt");
    let ResponsePayload::PromptAccepted(accepted) = accepted else {
        panic!("unexpected payload");
    };
    wait_run_terminal(&harness.client, &accepted.run_id).await;
    let plans = harness
        .client
        .request(Command::PlansList(PlanListParams {
            conversation_id: parent,
        }))
        .await
        .expect("plans");
    let ResponsePayload::PlansListed(plans) = plans else {
        panic!("unexpected payload");
    };
    assert_eq!(plans.len(), 1, "repeated update_plan must not add drafts");
}
