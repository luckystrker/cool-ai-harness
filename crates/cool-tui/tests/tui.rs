use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{
    AgentRuntime, ModelEvent, ScriptedDriver, Tool, ToolCall, ToolContext, ToolDefinition,
    ToolError, ToolHandler, ToolResult, builtin_registry,
};
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::{ApprovalDecision, CanonicalEvent, ClientState, GoldenTrace};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use cool_tui::{TuiApp, TuiCommand, TuiEvent, TuiKey, TuiState};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

struct SlowTool;

#[async_trait]
impl ToolHandler for SlowTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        tokio::time::sleep(Duration::from_millis(400)).await;
        Ok(ToolResult::ok(json!("slow")))
    }
}

fn write_tool_scripts() -> Vec<Result<Vec<ModelEvent>, cool_agent::ProviderError>> {
    vec![
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "write-1".to_owned(),
                name: "write_file".to_owned(),
                arguments: [("path".to_owned(), json!("note.txt"))]
                    .into_iter()
                    .chain([("content".to_owned(), json!("hi"))])
                    .collect(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("wrote the note".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "slow-1".to_owned(),
                name: "slow_tool".to_owned(),
                arguments: Default::default(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("should never be reached".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]
}

async fn tui_harness(
    directory: &std::path::Path,
) -> (
    TuiApp<TestBackend>,
    AppServer,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    tui_harness_with(directory, write_tool_scripts()).await
}

async fn tui_harness_with(
    directory: &std::path::Path,
    scripts: Vec<Result<Vec<ModelEvent>, cool_agent::ProviderError>>,
) -> (
    TuiApp<TestBackend>,
    AppServer,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let provider = Arc::new(ScriptedDriver::new(scripts));
    let mut registry = builtin_registry();
    registry = registry
        .extend([Tool::new(
            ToolDefinition {
                name: "slow_tool".to_owned(),
                description: "slow deterministic fixture".to_owned(),
                parameters: json!({"type": "object"}),
            },
            [],
            Decision::Allow,
            SlowTool,
        )])
        .unwrap();
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, registry),
        Workspace::new(directory).unwrap(),
        CapabilityPolicy::new(Some(Decision::Ask)),
        "scripted",
    )
    .unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn({
        let server = server.clone();
        async move { server.serve_io(server_io).await }
    });
    let client = AppClient::connect(reader, writer).unwrap();
    let terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let app = TuiApp::new(terminal, client, 80, 24);
    (app, server, task)
}

async fn type_text(app: &mut TuiApp<TestBackend>, text: &str) {
    for character in text.chars() {
        app.handle(TuiEvent::Key(TuiKey::Char(character)))
            .await
            .unwrap();
    }
}

async fn submit(app: &mut TuiApp<TestBackend>) {
    app.handle(TuiEvent::Key(TuiKey::Enter)).await.unwrap();
}

async fn pump_until(
    app: &mut TuiApp<TestBackend>,
    events: &mut broadcast::Receiver<cool_protocol::EventEnvelope>,
    description: &str,
    predicate: impl Fn(&TuiState) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !predicate(&app.state) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        match timeout(Duration::from_millis(250), events.recv()).await {
            Ok(Ok(envelope)) => {
                app.handle(TuiEvent::Server(Box::new(envelope)))
                    .await
                    .unwrap();
            }
            Ok(Err(_)) => panic!("event stream closed while waiting for {description}"),
            Err(_) => {}
        }
    }
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn prompt_requires_a_session_and_reports_a_notice() {
    let mut state = TuiState::new();
    state.submit();
    assert!(state.notice.is_none());
    state.input = "hello".to_owned();
    state.cursor = 5;
    let commands = state.submit();
    assert!(commands.is_empty());
    assert_eq!(
        state.notice.as_deref(),
        Some("no session selected; use /new or press Tab")
    );
}

#[test]
fn busy_state_routes_input_to_steering_and_approval_keys_resolve() {
    let mut state = TuiState::new();
    state.set_connected_session("session-1".to_owned());
    state.busy = true;
    state.run_id = Some("run-1".to_owned());
    state.input = "focus on the parser".to_owned();
    state.cursor = state.input.chars().count();
    let commands = state.submit();
    assert_eq!(
        commands,
        [TuiCommand::Steer {
            run_id: "run-1".to_owned(),
            text: "focus on the parser".to_owned(),
        }]
    );

    state.set_approval(&cool_protocol::ToolApprovalRequired {
        call_id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        arguments: [("path".to_owned(), json!("note.txt"))]
            .into_iter()
            .collect(),
        reason: "write capability".to_owned(),
        approval_id: "approval-1".to_owned(),
        revision: 3,
        breakpoint_type: None,
        result_preview: None,
        current_content: None,
    });
    let commands = state.on_key(TuiKey::Char('a'));
    assert_eq!(
        commands,
        [TuiCommand::ResolveApproval {
            approval_id: "approval-1".to_owned(),
            revision: 3,
            decision: ApprovalDecision::Approved,
        }]
    );
    assert!(state.approval.is_none());

    state.set_approval(&cool_protocol::ToolApprovalRequired {
        call_id: "call-2".to_owned(),
        name: "bash".to_owned(),
        arguments: Default::default(),
        reason: "execute capability".to_owned(),
        approval_id: "approval-2".to_owned(),
        revision: 1,
        breakpoint_type: None,
        result_preview: None,
        current_content: None,
    });
    let commands = state.on_key(TuiKey::Esc);
    assert!(matches!(
        commands.as_slice(),
        [TuiCommand::ResolveApproval {
            decision: ApprovalDecision::Denied,
            ..
        }]
    ));
}

#[test]
fn escape_cancels_a_run_and_quits_only_when_idle() {
    let mut state = TuiState::new();
    state.busy = true;
    state.run_id = Some("run-1".to_owned());
    assert_eq!(
        state.on_key(TuiKey::Esc),
        [TuiCommand::Cancel {
            run_id: "run-1".to_owned()
        }]
    );
    assert!(!state.should_quit);
    state.busy = false;
    assert!(state.on_key(TuiKey::Ctrl('c')).is_empty());
    assert!(state.should_quit);
}

#[test]
fn paste_and_slash_commands_shape_state() {
    let mut state = TuiState::new();
    state.set_connected_session("session-1".to_owned());
    state.on_key(TuiKey::Paste("first\nsecond".to_owned()));
    assert_eq!(state.input, "first second");
    assert_eq!(state.cursor, "first second".chars().count());

    state.input = "/model gpt-5".to_owned();
    state.cursor = state.input.chars().count();
    assert!(state.submit().is_empty());
    assert_eq!(state.model.as_deref(), Some("gpt-5"));

    state.input = "/help".to_owned();
    state.cursor = 5;
    state.submit();
    assert_eq!(state.mode, cool_tui::TuiMode::Help);
    assert!(matches!(state.on_key(TuiKey::Esc), commands if commands.is_empty()));
    assert_eq!(state.mode, cool_tui::TuiMode::Chat);

    state.input = "/unknown".to_owned();
    state.cursor = 8;
    state.submit();
    assert_eq!(state.notice.as_deref(), Some("unknown command: /unknown"));
}

#[test]
fn golden_traces_reduce_to_the_expected_client_state_through_the_tui() {
    let directory =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cool-protocol/tests/golden");
    let mut files = std::fs::read_dir(&directory)
        .expect("golden directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 12, "M1 requires all critical golden scenarios");
    for file in files {
        let trace: GoldenTrace =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("golden trace"))
                .expect("valid trace");
        let mut state = TuiState::new();
        for envelope in &trace.events {
            state.on_event(envelope);
        }
        assert_eq!(
            serde_json::to_value(&state.canonical).expect("canonical state"),
            serde_json::to_value(&trace.expected_state).expect("expected state"),
            "TUI reducer drift in {}",
            file.display()
        );
    }
}

#[tokio::test]
async fn interactive_run_with_approval_and_cancellation_renders_in_the_tui() {
    let directory = tempdir().unwrap();
    let (mut app, server, task) = tui_harness(directory.path()).await;
    let mut events = app.event_receiver();
    app.bootstrap().await.unwrap();
    assert!(
        app.state.session_id.is_some(),
        "bootstrap creates a session"
    );

    type_text(&mut app, "write a note").await;
    submit(&mut app).await;
    assert!(app.state.busy);
    pump_until(&mut app, &mut events, "approval", |state| {
        state.approval.is_some()
    })
    .await;
    let approval = app.state.approval.clone().unwrap();
    assert_eq!(approval.name, "write_file");
    let rendered = rendered_text(&app.terminal);
    assert!(rendered.contains("approval"), "approval pane is visible");
    assert!(rendered.contains("write_file"));

    app.handle(TuiEvent::Key(TuiKey::Char('a'))).await.unwrap();
    pump_until(&mut app, &mut events, "completion", |state| !state.busy).await;
    assert_eq!(app.state.canonical.run_status.as_deref(), Some("completed"));
    let rendered = rendered_text(&app.terminal);
    assert!(
        rendered.contains("wrote the note"),
        "assistant output is rendered: {rendered}"
    );

    // Resize and paste are handled without losing the conversation.
    app.handle(TuiEvent::Resize(120, 40)).await.unwrap();
    assert_eq!((app.width, app.height), (120, 40));
    app.handle(TuiEvent::Key(TuiKey::Paste("draft".to_owned())))
        .await
        .unwrap();
    assert_eq!(app.state.input, "draft");
    app.handle(TuiEvent::Key(TuiKey::Backspace)).await.unwrap();
    assert_eq!(app.state.input, "draf");

    // A second run stays active inside the slow tool and is cancelled from the
    // TUI with Escape.
    type_text(&mut app, "run the slow tool").await;
    submit(&mut app).await;
    pump_until(&mut app, &mut events, "tool start", |state| {
        state
            .canonical
            .tools
            .values()
            .any(|status| status == "running")
    })
    .await;
    app.handle(TuiEvent::Key(TuiKey::Esc)).await.unwrap();
    pump_until(&mut app, &mut events, "cancellation", |state| !state.busy).await;
    assert_eq!(app.state.canonical.run_status.as_deref(), Some("cancelled"));
    assert!(
        server
            .store()
            .run(app.state.run_id.as_deref().unwrap(), "local-user")
            .is_ok_and(|run| run.status == cool_state::RunStatus::Cancelled)
    );

    // The canonical reducer sees exactly the events the durable log holds.
    let run_id = app.state.run_id.clone().unwrap();
    let durable = server.events_for_run(&run_id).await.unwrap();
    let replay = ClientState::replay(&durable);
    assert_eq!(replay.run_status.as_deref(), Some("cancelled"));
    assert_eq!(app.state.canonical_run_id.as_deref(), Some(run_id.as_str()));

    drop(app);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn disconnected_events_are_visible_and_shutdown_cancels_the_run() {
    let directory = tempdir().unwrap();
    let scripts = vec![
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "slow-1".to_owned(),
                name: "slow_tool".to_owned(),
                arguments: Default::default(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("never reached".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ];
    let (mut app, server, task) = tui_harness_with(directory.path(), scripts).await;
    let mut events = app.event_receiver();
    app.bootstrap().await.unwrap();

    app.handle(TuiEvent::Disconnected("pipe closed".to_owned()))
        .await
        .unwrap();
    assert!(
        app.state
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("pipe closed"))
    );
    assert!(!app.state.busy);

    type_text(&mut app, "run the slow tool").await;
    submit(&mut app).await;
    pump_until(&mut app, &mut events, "tool start", |state| {
        state
            .canonical
            .tools
            .values()
            .any(|status| status == "running")
    })
    .await;

    let (sender, receiver) = tokio::sync::mpsc::channel::<TuiEvent>(8);
    let run_id = app.state.run_id.clone().unwrap();
    let loop_client = app.client.clone();
    let loop_task = tokio::spawn(async move {
        cool_tui::run_event_loop(app, receiver, loop_client.subscribe()).await
    });
    sender.send(TuiEvent::Key(TuiKey::Char('q'))).await.unwrap();
    drop(sender);
    loop_task.await.expect("loop task").expect("loop result");
    timeout(Duration::from_secs(5), async {
        loop {
            if server
                .store()
                .run(&run_id, "local-user")
                .is_ok_and(|run| run.status == cool_state::RunStatus::Cancelled)
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("shutdown cancellation becomes durable");
    task.await.expect("server task").expect("clean disconnect");
}

#[test]
fn plan_progress_is_rendered_from_the_canonical_plan_state() {
    let mut state = TuiState::new();
    let mut envelope = cool_protocol::EventEnvelope {
        event_id: "plan-1".to_owned(),
        schema_version: cool_protocol::V1Version::VALUE,
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        item_id: None,
        seq: 1,
        occurred_at: "2026-09-17T00:00:00Z".to_owned(),
        actor: cool_protocol::ActorRef {
            id: "cool-app-server".to_owned(),
            kind: cool_protocol::ActorKind::System,
        },
        source: "test".to_owned(),
        causation_id: None,
        correlation_id: None,
        event: CanonicalEvent::PlanCreated(cool_protocol::PlanCreated {
            plan_id: "plan-1".to_owned(),
            title: Some("Ship".to_owned()),
            total_steps: 2,
        }),
        extensions: Default::default(),
    };
    state.on_event(&envelope);
    envelope.seq = 2;
    envelope.event_id = "plan-2".to_owned();
    envelope.event = CanonicalEvent::PlanProgress(cool_protocol::PlanProgress {
        plan_id: "plan-1".to_owned(),
        completed_steps: 1,
        total_steps: 2,
        message: None,
        status: cool_protocol::PlanProgressStatus::Executing,
    });
    state.on_event(&envelope);

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| cool_tui::ui::render(frame, &state))
        .unwrap();
    let rendered = rendered_text(&terminal);
    assert!(
        rendered.contains("plan plan-1: 1/2 running"),
        "plan progress is rendered: {rendered}"
    );
}

#[test]
fn key_conversion_covers_resize_paste_and_modifiers() {
    use cool_tui::TuiKey as Key;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    let ctrl_c = cool_tui::convert_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL));
    assert_eq!(ctrl_c, Key::Ctrl('c'));
    let release = cool_tui::convert_key(KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: ratatui::crossterm::event::KeyEventState::NONE,
    });
    assert_eq!(release, Key::None);
    assert_eq!(
        cool_tui::convert_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
        Key::None
    );
}

#[tokio::test]
async fn unknown_events_and_other_run_replays_do_not_corrupt_state() {
    let directory = tempdir().unwrap();
    let (mut app, _server, task) = tui_harness(directory.path()).await;
    app.bootstrap().await.unwrap();
    let mut events = app.event_receiver();
    type_text(&mut app, "write a note").await;
    submit(&mut app).await;
    pump_until(&mut app, &mut events, "completion", |state| {
        state.canonical.last_seq == Some(5)
    })
    .await;
    let before = app.state.canonical.clone();
    let mut foreign = cool_protocol::EventEnvelope {
        event_id: "foreign-1".to_owned(),
        schema_version: cool_protocol::V1Version::VALUE,
        session_id: "session-other".to_owned(),
        run_id: "run-other".to_owned(),
        item_id: None,
        seq: 7,
        occurred_at: "2026-09-17T00:00:00Z".to_owned(),
        actor: cool_protocol::ActorRef {
            id: "cool-app-server".to_owned(),
            kind: cool_protocol::ActorKind::System,
        },
        source: "test".to_owned(),
        causation_id: None,
        correlation_id: None,
        event: CanonicalEvent::ContentDelta(cool_protocol::TextDelta {
            text: "foreign".to_owned(),
            channel: None,
        }),
        extensions: Default::default(),
    };
    app.handle(TuiEvent::Server(Box::new(foreign.clone())))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&app.state.canonical).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    foreign.seq = 1;
    foreign.event = CanonicalEvent::RunStarted(cool_protocol::RunStarted {
        model: None,
        mode: None,
    });
    app.handle(TuiEvent::Server(Box::new(foreign)))
        .await
        .unwrap();
    assert_eq!(
        app.state.canonical_run_id.as_deref(),
        Some("run-other"),
        "a fresh sequence adopts the new run"
    );
    drop(app);
    task.await.expect("server task").expect("clean disconnect");
}
