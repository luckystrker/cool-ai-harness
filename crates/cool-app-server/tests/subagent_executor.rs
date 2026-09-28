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

// ---------------------------------------------------------------------------
// P1.7: background subagent execution — fork_context, isolation=worktree and
// operator steers (send_to_subagent) draining into a running child.
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use async_trait::async_trait;
use cool_agent::{
    HostContext, HostLauncher, Message, MessageRole, ModelDriver, ModelStream, ProviderError,
};
use cool_app_server::{ForkContext, SubagentExecutor, SubagentIsolation, SubagentLaunchSpec};
use cool_store::domains::conversations::NewMessage;
use futures_util::stream;

fn direct_executor(
    driver: Arc<dyn ModelDriver>,
) -> (Arc<SubagentExecutor>, Arc<LegacyStore>, tempfile::TempDir) {
    let directory = tempdir().expect("tempdir");
    let store = Arc::new(LegacyStore::in_memory().expect("store"));
    store.ensure_actor("local-user").expect("actor");
    let executor = Arc::new(SubagentExecutor::new(
        store.clone(),
        DurableStore::in_memory().expect("durable"),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(directory.path()).expect("workspace"),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted".to_owned(),
        HostContext::default(),
    ));
    (executor, store, directory)
}

async fn executor_wait_terminal(executor: &SubagentExecutor, run_id: i64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row = executor.get_run("local-user", run_id).expect("run row");
        if ["completed", "failed", "cancelled"].contains(&row.status.as_str()) {
            return row.status;
        }
        assert!(std::time::Instant::now() < deadline, "run did not finish");
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn fork_context_full_seeds_the_child_history() {
    let driver = Arc::new(ScriptedDriver::echo());
    let (executor, store, _dir) = direct_executor(driver.clone());
    let parent = parent_conversation(&store);
    let run = executor
        .launch(
            "local-user",
            SubagentLaunchSpec {
                parent_conversation_id: parent,
                prompt: "continue".to_owned(),
                fork_context: ForkContext::Full,
                parent_history: vec![
                    Message::text(MessageRole::User, "earlier question"),
                    Message::text(MessageRole::Assistant, "earlier answer"),
                ],
                ..SubagentLaunchSpec::default()
            },
            "fork-full-key",
            "fingerprint",
        )
        .await
        .expect("launch");
    assert_eq!(executor_wait_terminal(&executor, run.id).await, "completed");

    let requests = driver.requests().await;
    assert_eq!(requests.len(), 1);
    let messages = &requests[0].messages;
    let texts: Vec<&str> = messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect();
    assert!(
        texts.contains(&"earlier question") && texts.contains(&"earlier answer"),
        "parent transcript seeded the child: {texts:?}"
    );
    assert_eq!(
        messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("continue"),
        "the spawn prompt rides last"
    );
}

#[tokio::test]
async fn fork_context_summary_folds_a_digest_into_the_system_prompt() {
    // First stream call is the summarizer (digest of the parent transcript),
    // second is the child run itself.
    let driver = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Content("digest: parent picked green".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]));
    let (executor, store, _dir) = direct_executor(driver.clone());
    let parent = parent_conversation(&store);
    let run = executor
        .launch(
            "local-user",
            SubagentLaunchSpec {
                parent_conversation_id: parent,
                prompt: "finish it".to_owned(),
                fork_context: ForkContext::Summary,
                parent_history: vec![Message::text(MessageRole::User, "pick a color")],
                ..SubagentLaunchSpec::default()
            },
            "fork-summary-key",
            "fingerprint",
        )
        .await
        .expect("launch");
    assert_eq!(executor_wait_terminal(&executor, run.id).await, "completed");

    let requests = driver.requests().await;
    assert_eq!(requests.len(), 2, "summarizer + child run");
    let system = requests[1]
        .messages
        .iter()
        .find(|message| message.role == MessageRole::System)
        .and_then(|message| message.content.clone())
        .unwrap_or_default();
    assert!(
        system.contains("digest: parent picked green"),
        "digest folded into the child system prompt: {system}"
    );
    assert!(
        system.contains("Parent conversation summary"),
        "forked-context marker present: {system}"
    );
}

/// Driver whose turn-1 stream emits its tool call and then holds the finish
/// event until the test's steer is written — the next iteration's
/// `drain_steers` then picks the steer up deterministically.
struct SteerGateDriver {
    requests: tokio::sync::Mutex<Vec<cool_agent::ModelRequest>>,
    calls: AtomicUsize,
    released: Arc<AtomicBool>,
}

fn scripted_stream(events: Vec<ModelEvent>) -> ModelStream {
    Box::pin(stream::iter(events.into_iter().map(Ok)))
}

#[async_trait]
impl ModelDriver for SteerGateDriver {
    async fn stream(
        &self,
        request: cool_agent::ModelRequest,
    ) -> Result<ModelStream, ProviderError> {
        self.requests.lock().await.push(request);
        if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            let released = self.released.clone();
            return Ok(Box::pin(stream::unfold(0_u8, move |step| {
                let released = released.clone();
                async move {
                    match step {
                        0 => Some((
                            Ok(ModelEvent::ToolCall(ToolCall {
                                call_id: "call-1".to_owned(),
                                name: "list_files".to_owned(),
                                arguments: serde_json::Map::from_iter([(
                                    "path".to_owned(),
                                    json!("."),
                                )]),
                            })),
                            1_u8,
                        )),
                        1 => {
                            for _ in 0..500 {
                                if released.load(AtomicOrdering::SeqCst) {
                                    break;
                                }
                                sleep(Duration::from_millis(10)).await;
                            }
                            Some((
                                Ok(ModelEvent::Finish {
                                    reason: Some("stop".to_owned()),
                                }),
                                2_u8,
                            ))
                        }
                        _ => None,
                    }
                }
            })));
        }
        Ok(scripted_stream(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]))
    }
}

#[tokio::test]
async fn send_to_subagent_steer_reaches_the_running_child() {
    let driver = Arc::new(SteerGateDriver {
        requests: tokio::sync::Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
        released: Arc::new(AtomicBool::new(false)),
    });
    let runtime_driver: Arc<dyn ModelDriver> = driver.clone();
    let (executor, store, _dir) = direct_executor(runtime_driver);
    let parent = parent_conversation(&store);
    let run = executor
        .launch(
            "local-user",
            SubagentLaunchSpec {
                parent_conversation_id: parent,
                prompt: "first step".to_owned(),
                ..SubagentLaunchSpec::default()
            },
            "steer-key",
            "fingerprint",
        )
        .await
        .expect("launch");

    // Wait for the first provider call (turn 1 returned the tool call), then
    // append the steer exactly like the send_to_subagent tool does.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !driver.requests.lock().await.is_empty() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no provider request");
        sleep(Duration::from_millis(10)).await;
    }
    store
        .add_message(
            "local-user",
            run.conversation_id,
            &NewMessage {
                role: "user".to_owned(),
                content: Some("steer: change of plan".to_owned()),
                ..NewMessage::default()
            },
        )
        .expect("steer message");
    driver.released.store(true, AtomicOrdering::SeqCst);

    assert_eq!(executor_wait_terminal(&executor, run.id).await, "completed");
    let requests = driver.requests.lock().await.clone();
    assert_eq!(requests.len(), 2, "tool turn + follow-up turn");
    let steered = requests[1].messages.iter().any(|message| {
        message.role == MessageRole::User
            && message.content.as_deref() == Some("steer: change of plan")
    });
    assert!(steered, "steer was drained into the child's history");
}

#[tokio::test]
async fn isolation_worktree_runs_the_child_in_a_git_worktree() {
    let directory = tempdir().expect("tempdir");
    // worktree add needs a commit to point the new branch at.
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "test"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(directory.path())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }
    let store = Arc::new(LegacyStore::in_memory().expect("store"));
    store.ensure_actor("local-user").expect("actor");
    let driver = Arc::new(ScriptedDriver::echo());
    let executor = Arc::new(SubagentExecutor::new(
        store.clone(),
        DurableStore::in_memory().expect("durable"),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(directory.path()).expect("workspace"),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted".to_owned(),
        HostContext {
            launcher: Arc::new(HostLauncher),
            environment: std::env::vars().collect(),
            ..HostContext::default()
        },
    ));
    let parent = parent_conversation(&store);
    let run = executor
        .launch(
            "local-user",
            SubagentLaunchSpec {
                parent_conversation_id: parent,
                prompt: "work in isolation".to_owned(),
                isolation: SubagentIsolation::Worktree,
                ..SubagentLaunchSpec::default()
            },
            "worktree-key",
            "fingerprint",
        )
        .await
        .expect("launch");
    let status = executor_wait_terminal(&executor, run.id).await;
    let row = executor.get_run("local-user", run.id).expect("row");
    assert_eq!(status, "completed", "run failed: {:?}", row.error);

    // Teardown (P1.7): the finished run's worktree and its `cool/sub/*`
    // branch are reclaimed — `.cool/worktrees/` must not accumulate litter.
    let worktree = directory
        .path()
        .join(".cool")
        .join("worktrees")
        .join(run.id.to_string());
    assert!(
        !worktree.exists(),
        "worktree removed after the run: {}",
        worktree.display()
    );
    let listed = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(directory.path())
        .output()
        .expect("git worktree list");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("worktrees"),
        "no worktree entries left: {listed:?}"
    );
    let branches = std::process::Command::new("git")
        .args(["branch", "--list", "cool/sub/*"])
        .current_dir(directory.path())
        .output()
        .expect("git branch --list");
    assert!(
        String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
        "cool/sub/* branches cleaned up: {branches:?}"
    );
}

#[tokio::test]
async fn isolation_worktree_fails_closed_when_no_launcher_is_configured() {
    let driver = Arc::new(ScriptedDriver::echo());
    let (executor, store, _dir) = direct_executor(driver); // DisabledLauncher
    let parent = parent_conversation(&store);
    let run = executor
        .launch(
            "local-user",
            SubagentLaunchSpec {
                parent_conversation_id: parent,
                prompt: "isolate me".to_owned(),
                isolation: SubagentIsolation::Worktree,
                ..SubagentLaunchSpec::default()
            },
            "worktree-no-launcher",
            "fingerprint",
        )
        .await
        .expect("launch");
    let status = executor_wait_terminal(&executor, run.id).await;
    assert_eq!(status, "failed");
    let row = executor.get_run("local-user", run.id).expect("row");
    assert!(
        row.error
            .as_deref()
            .unwrap_or_default()
            .contains("launcher"),
        "launcher-disabled error is recorded: {:?}",
        row.error
    );
}

#[tokio::test]
async fn reviewer_profile_is_builtin_read_only_with_git_restricted() {
    // P2.15: the seeded reviewer profile keeps file reads + git only, and its
    // exec_rules let `git diff`/`git log` through while `git push` is denied
    // (a capability `deny` can't be lifted by rules, so `execute` stays ask).
    let driver = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "push-call".to_owned(),
                name: "git".to_owned(),
                arguments: serde_json::Map::from_iter([(
                    "args".to_owned(),
                    json!(["push", "origin", "main"]),
                )]),
            }),
            ModelEvent::ToolCall(ToolCall {
                call_id: "diff-call".to_owned(),
                name: "git".to_owned(),
                arguments: serde_json::Map::from_iter([(
                    "args".to_owned(),
                    json!(["diff", "HEAD"]),
                )]),
            }),
            // A registered but unlisted tool must fail at execution — the
            // profile allowlist gates calls, not just advertised definitions.
            ModelEvent::ToolCall(ToolCall {
                call_id: "write-call".to_owned(),
                name: "write_file".to_owned(),
                arguments: serde_json::Map::from_iter([("path".to_owned(), json!("owned.md"))]),
            }),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
        Ok(vec![
            ModelEvent::Content("reviewed".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]));
    let (executor, store, _dir) = direct_executor(driver.clone());
    store
        .seed_builtin_profiles()
        .expect("builtin profiles seed");
    let reviewer = store
        .list_profiles(false)
        .expect("profiles")
        .into_iter()
        .find(|profile| profile.slug == "reviewer")
        .expect("reviewer preset exists");
    assert!(reviewer.is_builtin, "reviewer is builtin");

    let parent = parent_conversation(&store);
    let run = executor
        .launch(
            "local-user",
            SubagentLaunchSpec {
                parent_conversation_id: parent,
                profile_id: Some(reviewer.id),
                prompt: "review the diff".to_owned(),
                ..SubagentLaunchSpec::default()
            },
            "reviewer-key",
            "fingerprint",
        )
        .await
        .expect("launch");
    assert_eq!(executor_wait_terminal(&executor, run.id).await, "completed");

    let requests = driver.requests().await;
    assert_eq!(requests.len(), 2);
    let mut tool_names: Vec<&str> = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    tool_names.sort_unstable();
    assert_eq!(
        tool_names,
        vec![
            "find_files",
            "git",
            "list_files",
            "read_file",
            "search_files"
        ],
        "reviewer sees the read-only tool set"
    );
    // Turn 2's history carries the tool results: `git push` denied by the
    // exec rule, `git diff` allowed through (fails only because no process
    // launcher exists in this harness).
    let tool_results = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        tool_results.contains("denied by policy rule"),
        "git push denied by the reviewer exec rules: {tool_results}"
    );
    let diff_result = requests[1]
        .messages
        .iter()
        .filter(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("diff-call")
        })
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !diff_result.contains("denied"),
        "git diff passed the exec rules: {diff_result}"
    );
    let write_result = requests[1]
        .messages
        .iter()
        .filter(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("write-call")
        })
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        write_result.contains("allowlist"),
        "unlisted write_file rejected by the tool_names gate: {write_result}"
    );
}
