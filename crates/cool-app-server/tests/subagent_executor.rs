//! M11 subagent executor coverage: launch/launch_batch/runs_cancel drive the
//! Rust agent runtime, the child transcript is persisted, and subagent
//! lifecycle events reach the parent conversation's canonical session.

use std::sync::Arc;
use std::time::Duration;

use cool_agent::{AgentRuntime, ModelEvent, ScriptedDriver, ToolCall, builtin_registry};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::*;
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::domains::conversations::{MessagePage, NewConversation};
use cool_store::domains::profiles::NewAgentProfile;
use cool_store::domains::subagents::NewSubagentRole;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::sleep;

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("valid key")
}

struct Harness {
    client: AppClient,
    store: Arc<LegacyStore>,
    driver: Arc<ScriptedDriver>,
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
        AgentRuntime::new(driver.clone(), builtin_registry()),
        Workspace::new(directory.path()).expect("workspace"),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .expect("server");
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client
        .initialize("subagent-tests", "1")
        .await
        .expect("init");
    Harness {
        client,
        store,
        driver,
        _task: task,
        _directory: directory,
    }
}

fn parent_conversation(store: &LegacyStore) -> i64 {
    store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent conversation")
        .id
}

async fn run_status(client: &AppClient, run_id: i64) -> ResponsePayload {
    client
        .request(Command::SubagentsRunsGet(LegacyIdParams { id: run_id }))
        .await
        .expect("run detail")
}

async fn wait_terminal(client: &AppClient, run_id: i64) -> SubagentRunRecord {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let detail = run_status(client, run_id).await;
        let ResponsePayload::SubagentsRunsGot(detail) = detail else {
            panic!("unexpected payload");
        };
        if ["completed", "failed", "cancelled"].contains(&detail.run.status.as_str()) {
            return detail.run;
        }
        assert!(std::time::Instant::now() < deadline, "run did not finish");
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn launch_executes_the_runtime_and_persists_the_child_transcript() {
    let driver = Arc::new(ScriptedDriver::echo());
    let harness = harness(driver).await;
    let parent = parent_conversation(&harness.store);
    let role = harness
        .store
        .create_subagent_role(&NewSubagentRole {
            name: "worker".to_owned(),
            system_prompt: Some("be terse".to_owned()),
            ..NewSubagentRole::default()
        })
        .expect("role");

    let launched = harness
        .client
        .request(Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("launch-1"),
            parent_conversation_id: parent,
            role_id: Some(role.id),
            profile_id: None,
            parent_run_id: None,
            name: Some("worker one".to_owned()),
            prompt: "inspect the diff".to_owned(),
            model: None,
        }))
        .await
        .expect("launch");
    let ResponsePayload::SubagentsLaunched(created) = launched else {
        panic!("unexpected payload: {launched:?}");
    };
    assert_eq!(created.status, "queued");
    assert!(created.run_id.is_some(), "child agent run is linked");

    let finished = wait_terminal(&harness.client, created.id).await;
    assert_eq!(finished.status, "completed");
    // The echo provider returns the last user message, so the child transcript
    // records the prompt as both the first user and the final assistant row.
    assert_eq!(finished.result_summary.as_deref(), Some("inspect the diff"));

    let messages = harness
        .store
        .list_messages(
            "local-user",
            created.conversation_id,
            &MessagePage {
                limit: Some(50),
                ..MessagePage::default()
            },
        )
        .expect("child transcript");
    assert_eq!(messages.len(), 2, "user prompt + assistant reply");
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content.as_deref(), Some("inspect the diff"));
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].content.as_deref(), Some("inspect the diff"));
}

#[tokio::test]
async fn launch_is_idempotent_and_does_not_create_a_second_child() {
    let driver = Arc::new(ScriptedDriver::echo());
    let harness = harness(driver).await;
    let parent = parent_conversation(&harness.store);
    let params = || SubagentLaunchParams {
        idempotency_key: key("replay"),
        parent_conversation_id: parent,
        role_id: None,
        profile_id: None,
        parent_run_id: None,
        name: Some("once".to_owned()),
        prompt: "hello".to_owned(),
        model: None,
    };
    let first = harness
        .client
        .request(Command::SubagentsLaunch(params()))
        .await
        .expect("launch");
    let ResponsePayload::SubagentsLaunched(first) = first else {
        panic!("unexpected payload");
    };
    let second = harness
        .client
        .request(Command::SubagentsLaunch(params()))
        .await
        .expect("replay");
    let ResponsePayload::SubagentsLaunched(second) = second else {
        panic!("unexpected payload");
    };
    assert_eq!(first.id, second.id);
    assert_eq!(first.conversation_id, second.conversation_id);

    let runs = harness
        .client
        .request(Command::SubagentsRunsList(SubagentRunListParams {
            parent_conversation_id: Some(parent),
            status: None,
            limit: 10,
        }))
        .await
        .expect("list");
    let ResponsePayload::SubagentsRunsListed(items) = runs else {
        panic!("unexpected payload");
    };
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn launch_batch_creates_and_executes_every_item() {
    let driver = Arc::new(ScriptedDriver::echo());
    let harness = harness(driver).await;
    let parent = parent_conversation(&harness.store);
    let launched = harness
        .client
        .request(Command::SubagentsLaunchBatch(SubagentLaunchBatchParams {
            idempotency_key: key("batch"),
            parent_conversation_id: parent,
            items: vec![
                SubagentLaunchItem {
                    role_id: None,
                    profile_id: None,
                    name: Some("a".to_owned()),
                    prompt: "first".to_owned(),
                    model: None,
                },
                SubagentLaunchItem {
                    role_id: None,
                    profile_id: None,
                    name: Some("b".to_owned()),
                    prompt: "second".to_owned(),
                    model: None,
                },
            ],
        }))
        .await
        .expect("batch");
    let ResponsePayload::SubagentsLaunchedBatch(runs) = launched else {
        panic!("unexpected payload");
    };
    assert_eq!(runs.len(), 2);
    for run in runs {
        let finished = wait_terminal(&harness.client, run.id).await;
        assert_eq!(finished.status, "completed");
    }
}

#[tokio::test]
async fn cancel_signals_a_live_run() {
    let driver = Arc::new(ScriptedDriver::echo_with_delay(Duration::from_millis(800)));
    let harness = harness(driver).await;
    let parent = parent_conversation(&harness.store);
    let launched = harness
        .client
        .request(Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("cancel-launch"),
            parent_conversation_id: parent,
            role_id: None,
            profile_id: None,
            parent_run_id: None,
            name: None,
            prompt: "long".to_owned(),
            model: None,
        }))
        .await
        .expect("launch");
    let ResponsePayload::SubagentsLaunched(run) = launched else {
        panic!("unexpected payload");
    };
    sleep(Duration::from_millis(50)).await;
    let cancelled = harness
        .client
        .request(Command::SubagentsRunsCancel(IdempotentIdParams {
            idempotency_key: key("cancel"),
            id: run.id,
        }))
        .await
        .expect("cancel");
    let ResponsePayload::SubagentsRunsCancelled(result) = cancelled else {
        panic!("unexpected payload");
    };
    assert!(result.cancelled);
    let finished = wait_terminal(&harness.client, run.id).await;
    assert_eq!(finished.status, "cancelled");

    // Prove the signal actually aborted the run: if the live cancel had been
    // ignored, the 800 ms provider would still emit its content and the child
    // transcript would gain an assistant row. The sink must see only the prompt.
    sleep(Duration::from_millis(1000)).await;
    let messages = harness
        .store
        .list_messages(
            "local-user",
            run.conversation_id,
            &MessagePage {
                limit: Some(50),
                ..MessagePage::default()
            },
        )
        .expect("child transcript");
    assert_eq!(
        messages.len(),
        1,
        "the cancelled run did not execute to completion: {messages:?}"
    );
    assert_eq!(messages[0].role, "user");
}

#[tokio::test]
async fn lifecycle_events_reach_the_linked_parent_session() {
    let driver = Arc::new(ScriptedDriver::echo());
    let harness = harness(driver).await;
    let parent = parent_conversation(&harness.store);
    let linked = harness
        .client
        .request(Command::SessionForConversation(
            SessionForConversationParams {
                idempotency_key: key("link"),
                conversation_id: parent,
            },
        ))
        .await
        .expect("link");
    let ResponsePayload::SessionForConversation(link) = linked else {
        panic!("unexpected payload");
    };
    let launched = harness
        .client
        .request(Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("lifecycle-launch"),
            parent_conversation_id: parent,
            role_id: None,
            profile_id: None,
            parent_run_id: None,
            name: None,
            prompt: "link me".to_owned(),
            model: None,
        }))
        .await
        .expect("launch");
    let ResponsePayload::SubagentsLaunched(run) = launched else {
        panic!("unexpected payload");
    };
    wait_terminal(&harness.client, run.id).await;

    let runs = harness
        .client
        .request(Command::SessionRuns(SessionRunsParams {
            session_id: link.session_id.clone(),
            limit: 10,
        }))
        .await
        .expect("session runs");
    let ResponsePayload::SessionRuns(listed) = runs else {
        panic!("unexpected payload");
    };
    let lifecycle = listed
        .runs
        .iter()
        .find(|run| run.finish_reason.as_deref() == Some("subagent_completed"))
        .expect("a subagent lifecycle run was projected into the parent session");
    let events = harness
        .client
        .request(Command::RunEvents(RunEventsParams {
            run_id: lifecycle.run_id.clone(),
            after_seq: None,
            limit: 50,
        }))
        .await
        .expect("run events");
    let ResponsePayload::EventPage(page) = events else {
        panic!("unexpected payload");
    };
    assert!(
        page.events
            .iter()
            .any(|envelope| matches!(envelope.event, CanonicalEvent::SubagentStarted(_))),
        "parent canonical log carries subagent.started"
    );
    assert!(
        page.events
            .iter()
            .any(|envelope| matches!(envelope.event, CanonicalEvent::SubagentCompleted(_))),
        "parent canonical log carries subagent.completed"
    );
}

#[tokio::test]
async fn role_and_profile_precedence_reach_the_provider() {
    let driver = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("done".to_owned()),
        ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        },
    ])]));
    let harness = harness(driver.clone()).await;
    let parent = parent_conversation(&harness.store);
    let role = harness
        .store
        .create_subagent_role(&NewSubagentRole {
            name: "role-worker".to_owned(),
            system_prompt: Some("role prompt".to_owned()),
            model: Some("role-model".to_owned()),
            tool_names: Some(json!(["read_file"])),
            ..NewSubagentRole::default()
        })
        .expect("role");
    let profile = harness
        .store
        .create_profile(&NewAgentProfile {
            name: "profile".to_owned(),
            slug: "profile".to_owned(),
            system_prompt: Some("profile prompt".to_owned()),
            model: Some("profile-model".to_owned()),
            tool_names: Some(json!(["list_files"])),
            ..NewAgentProfile::default()
        })
        .expect("profile");

    harness
        .client
        .request(Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("precedence"),
            parent_conversation_id: parent,
            role_id: Some(role.id),
            profile_id: Some(profile.id),
            parent_run_id: None,
            name: None,
            prompt: "run".to_owned(),
            model: None,
        }))
        .await
        .expect("launch");
    // Wait for the provider request to be issued.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let request = loop {
        let requests = harness.driver.requests().await;
        if let Some(request) = requests.into_iter().next() {
            break request;
        }
        assert!(std::time::Instant::now() < deadline, "no provider request");
        sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(request.model, "profile-model");
    let system = request
        .messages
        .iter()
        .find(|message| message.role == cool_agent::MessageRole::System)
        .and_then(|message| message.content.clone())
        .unwrap_or_default();
    assert!(
        system.contains("profile prompt"),
        "profile prompt wins: {system}"
    );
    let tool_names = request
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(tool_names, vec!["list_files".to_owned()]);
}

#[tokio::test]
async fn child_transcript_masks_a_secret_in_tool_arguments() {
    // A model that echoes a credential into tool arguments must not have it
    // persisted verbatim in the child transcript.
    let driver = Arc::new(ScriptedDriver::new([
        Ok(vec![ModelEvent::ToolCall(ToolCall {
            call_id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::Map::from_iter([(
                "path".to_owned(),
                json!("sk-abcdefghijklmnopqrstuvwxyz"),
            )]),
        })]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]));
    let harness = harness(driver).await;
    let parent = parent_conversation(&harness.store);
    let launched = harness
        .client
        .request(Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("mask"),
            parent_conversation_id: parent,
            role_id: None,
            profile_id: None,
            parent_run_id: None,
            name: None,
            prompt: "read it".to_owned(),
            model: None,
        }))
        .await
        .expect("launch");
    let ResponsePayload::SubagentsLaunched(run) = launched else {
        panic!("unexpected payload");
    };
    wait_terminal(&harness.client, run.id).await;

    let messages = harness
        .store
        .list_messages(
            "local-user",
            run.conversation_id,
            &MessagePage {
                limit: Some(50),
                ..MessagePage::default()
            },
        )
        .expect("child transcript");
    let stored = messages
        .iter()
        .filter(|message| message.tool_calls.is_some())
        .map(|message| serde_json::to_string(&message.tool_calls).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!stored.is_empty(), "assistant tool call was persisted");
    assert!(
        !stored.contains("sk-abcdefghijklmnopqrstuvwxyz"),
        "raw secret leaked into the child transcript: {stored}"
    );
    assert!(
        stored.contains("[REDACTED:api-key]"),
        "secret was not masked: {stored}"
    );
}

#[tokio::test]
async fn an_invalid_working_directory_fails_closed() {
    let driver = Arc::new(ScriptedDriver::echo());
    let harness = harness(driver).await;
    let parent = harness
        .store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("Parent".to_owned()),
                working_directory: Some("Z:/definitely/missing".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("parent")
        .id;
    let launched = harness
        .client
        .request(Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("bad-workdir"),
            parent_conversation_id: parent,
            role_id: None,
            profile_id: None,
            parent_run_id: None,
            name: None,
            prompt: "run".to_owned(),
            model: None,
        }))
        .await
        .expect("launch");
    let ResponsePayload::SubagentsLaunched(run) = launched else {
        panic!("unexpected payload");
    };
    let finished = wait_terminal(&harness.client, run.id).await;
    assert_eq!(finished.status, "failed");
    assert_eq!(finished.error.as_deref(), Some("invalid working directory"));
}
