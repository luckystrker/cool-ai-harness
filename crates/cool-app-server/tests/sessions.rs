use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{
    AgentRuntime, ModelEvent, ScriptedDriver, Tool, ToolCall, ToolContext, ToolDefinition,
    ToolError, ToolHandler, ToolResult, builtin_registry,
};
use cool_app_server::client::new_idempotency_key;
use cool_app_server::{
    AppClient, AppServer, AppSettings, ExtensionAdmin, McpAdmin, RunLifecycle, ServerConfig,
    SkillAdmin,
};
use cool_protocol::{
    CanonicalEvent, Command, EmptyParams, ExtensionDiagnosticRecord, ExtensionStatusResult,
    HookRecord, HookReviewParams, McpAddServerParams, McpConnectResult, McpHealthResult,
    McpServerAdminRecord, McpServerListResult, McpServerNameParams, McpServerRecord,
    McpToolListResult, McpToolPolicyRecord, McpToolRecord, McpUpdateServerParams,
    PluginEnabledParams, PluginRecord, ResponsePayload, SkillAdminRecord, SkillCreateParams,
    SkillCreateResult, SkillListResult, SkillRecord, StatusEntry, StatusGetResult,
    SystemPromptRecord, SystemPromptSetParams, WorkerRecord,
};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use serde_json::json;
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

fn key(prefix: &str) -> cool_protocol::IdempotencyKey {
    cool_protocol::IdempotencyKey::new(new_idempotency_key(prefix)).unwrap()
}

struct SlowTool;

#[async_trait]
impl ToolHandler for SlowTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(ToolResult::ok(json!("slow result")))
    }
}

/// Blocks inside a tool call until released, so a test can hold a run live
/// deterministically instead of relying on provider timing.
struct GateTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ToolHandler for GateTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolResult::ok(json!("released")))
    }
}

async fn connected_client(
    server: AppServer,
) -> (AppClient, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client.initialize("session-tests", "1").await.unwrap();
    (client, task)
}

fn scripted_server(driver: Arc<ScriptedDriver>, workspace: &std::path::Path) -> AppServer {
    AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(workspace).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap()
}

async fn drain_run(
    mut events: tokio::sync::broadcast::Receiver<cool_protocol::EventEnvelope>,
    run_id: &str,
) -> Vec<CanonicalEvent> {
    let mut collected = Vec::new();
    loop {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event arrival timeout")
            .expect("event channel open");
        if envelope.run_id != run_id {
            continue;
        }
        let terminal = matches!(
            envelope.event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        );
        collected.push(envelope.event);
        if terminal {
            return collected;
        }
    }
}

#[tokio::test]
async fn session_list_history_and_fork_are_protocol_visible() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Content("first".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("second".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server.clone()).await;

    let session_id = client
        .create_session("list-key", Some("listed"), Some("project-a"))
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt("list-prompt", &session_id, "question", None)
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;

    let listed = client.list_sessions(Some("project-a"), 10).await.unwrap();
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].session_id, session_id);
    assert_eq!(listed.sessions[0].title.as_deref(), Some("listed"));
    assert_eq!(listed.sessions[0].project_key.as_deref(), Some("project-a"));
    assert!(
        client
            .list_sessions(Some("project-b"), 10)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    assert!(matches!(
        client.list_sessions(None, 0).await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "invalid_session_list_limit"
    ));

    let history = client.session_history(&session_id, 50).await.unwrap();
    assert!(!history.has_more);
    let roles = history
        .items
        .iter()
        .map(|item| item.role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(roles, ["user", "assistant"]);
    assert_eq!(history.items[0].content.as_deref(), Some("question"));
    assert_eq!(history.items[1].content.as_deref(), Some("first"));

    let forked = client
        .fork_session("fork-key", &session_id, Some("branch"))
        .await
        .unwrap();
    assert_eq!(forked.forked_from, session_id);
    assert_ne!(forked.session_id, session_id);
    let forked_history = client
        .session_history(&forked.session_id, 50)
        .await
        .unwrap();
    assert_eq!(
        forked_history
            .items
            .iter()
            .map(|item| item.content.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["question", "first"]
    );
    let replay = client
        .fork_session("fork-key", &session_id, Some("branch"))
        .await
        .unwrap();
    assert_eq!(replay.session_id, forked.session_id);

    let forked_events = client.subscribe();
    let forked_run = client
        .prompt("fork-prompt", &forked.session_id, "follow-up", None)
        .await
        .unwrap()
        .run_id;
    let forked_events = drain_run(forked_events, &forked_run).await;
    assert!(matches!(
        forked_events.last().unwrap(),
        CanonicalEvent::RunCompleted(_)
    ));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn session_steer_is_durable_and_reaches_the_next_model_request() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(ToolCall {
                call_id: "call-1".to_owned(),
                name: "slow_tool".to_owned(),
                arguments: Default::default(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("finished".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
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
        AgentRuntime::new(provider.clone(), registry),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;
    let session_id = client
        .create_session("steer-session", None, None)
        .await
        .unwrap();
    let mut events = client.subscribe();
    let run_id = client
        .prompt("steer-prompt", &session_id, "start", None)
        .await
        .unwrap()
        .run_id;

    // Steer while the slow tool keeps the run active so the message is drained
    // at the next iteration boundary.
    let steer = client
        .steer("steer-key", &run_id, "use the other file")
        .await
        .unwrap();
    assert_eq!(steer.run_id, run_id);
    assert!(steer.seq >= 3);
    let steer_seq = steer.seq;

    let replay = client
        .steer("steer-key", &run_id, "use the other file")
        .await
        .unwrap();
    assert_eq!(replay.seq, steer_seq);
    assert!(matches!(
        client.steer("steer-key", &run_id, "different").await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "idempotency_conflict"
    ));
    assert!(matches!(
        client
            .steer(&new_idempotency_key("empty"), &run_id, "   ")
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "empty_steer_content"
    ));

    let mut terminal = false;
    while !terminal {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event arrival timeout")
            .expect("event channel open");
        if envelope.run_id != run_id {
            continue;
        }
        terminal = matches!(
            envelope.event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        );
    }
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2);
    let second = requests[1]
        .messages
        .iter()
        .filter_map(|message| message.content.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        second
            .iter()
            .filter(|content| content.as_str() == "use the other file")
            .count(),
        1,
        "steer is folded into the model history exactly once"
    );

    let events = server.events_for_run(&run_id).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.event,
        CanonicalEvent::ItemCompleted(item)
            if item.content.as_deref() == Some("use the other file")
    )));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn steer_is_rejected_for_terminal_and_foreign_runs() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("done".to_owned()),
        ModelEvent::Finish { reason: None },
    ])]));
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server.clone()).await;
    let session_id = client
        .create_session("terminal-session", None, None)
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt("terminal-prompt", &session_id, "hello", None)
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;
    assert!(matches!(
        client
            .steer(&new_idempotency_key("late"), &run_id, "too late")
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "run_not_active"
    ));
    assert!(matches!(
        client
            .steer(&new_idempotency_key("missing"), "run-missing", "hello")
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "run_not_found"
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

struct StaticStatus;

#[async_trait]
impl RunLifecycle for StaticStatus {
    async fn on_event(
        &self,
        _event: &str,
        _payload: serde_json::Value,
        _policy: &CapabilityPolicy,
    ) -> Vec<CanonicalEvent> {
        Vec::new()
    }

    async fn status(&self) -> Option<StatusGetResult> {
        Some(StatusGetResult {
            plugins: vec![StatusEntry {
                id: "demo".to_owned(),
                status: "enabled".to_owned(),
                code: None,
            }],
            workers: vec![StatusEntry {
                id: "worker-1".to_owned(),
                status: "running".to_owned(),
                code: None,
            }],
            mcp_servers: vec!["demo/files".to_owned()],
        })
    }
}

#[tokio::test]
async fn status_get_reports_the_configured_lifecycle_snapshot() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;
    let empty = client.status().await.unwrap();
    assert!(empty.plugins.is_empty());
    assert!(empty.workers.is_empty());
    assert!(empty.mcp_servers.is_empty());
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    let server = server.with_run_lifecycle(Arc::new(StaticStatus));
    let (client, task) = connected_client(server).await;
    let status = client.status().await.unwrap();
    assert_eq!(status.plugins[0].id, "demo");
    assert_eq!(status.plugins[0].status, "enabled");
    assert_eq!(status.workers[0].status, "running");
    assert_eq!(status.mcp_servers, ["demo/files"]);
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

struct StaticExtensionAdmin;

#[async_trait]
impl ExtensionAdmin for StaticExtensionAdmin {
    async fn status(&self) -> Result<ExtensionStatusResult, String> {
        Ok(ExtensionStatusResult {
            plugins: vec![PluginRecord {
                name: "demo".to_owned(),
                version: "1.2.0".to_owned(),
                enabled: true,
                source_type: "local".to_owned(),
                source: "fixtures/demo".to_owned(),
                revision: String::new(),
                content_hash: "a".repeat(64),
                installed_at: "2026-09-20T00:00:00.000Z".to_owned(),
                required_capabilities: vec!["execute".to_owned()],
                resolved_dependencies: vec!["git".to_owned()],
                diagnostics: vec![ExtensionDiagnosticRecord {
                    code: "vendor_translated".to_owned(),
                    level: "info".to_owned(),
                    message: "translated".to_owned(),
                    path: "mcp.json".to_owned(),
                }],
                content_verified: true,
            }],
            workers: vec![WorkerRecord {
                id: "opencode-compatibility".to_owned(),
                status: "failed".to_owned(),
                attempt: 2,
                code: Some("crash".to_owned()),
            }],
            hooks: vec![HookRecord {
                plugin: "demo".to_owned(),
                id: "audit".to_owned(),
                event: "PreToolUse".to_owned(),
                handler: "command:/usr/bin/audit".to_owned(),
                order: 0,
                parallel: false,
                capabilities: vec!["execute".to_owned()],
                trust_hash: "b".repeat(64),
                approved: false,
            }],
            skills: vec![SkillRecord {
                name: "changelog".to_owned(),
                description: "Write a changelog".to_owned(),
                plugin: "demo".to_owned(),
                allowed_tools: vec!["read_file".to_owned()],
            }],
            mcp_servers: vec![McpServerRecord {
                plugin: "demo".to_owned(),
                name: "files".to_owned(),
                transport: "stdio".to_owned(),
                endpoint: "/usr/bin/mcp-files".to_owned(),
            }],
            mcp_tool_policy: McpToolPolicyRecord {
                enabled: Some(vec!["read".to_owned()]),
                disabled: vec!["write".to_owned()],
            },
        })
    }
}

struct FailingExtensionAdmin;

#[async_trait]
impl ExtensionAdmin for FailingExtensionAdmin {
    async fn status(&self) -> Result<ExtensionStatusResult, String> {
        Err("plugin store is invalid: boom".to_owned())
    }
}

#[derive(Default)]
struct MutatingExtensionAdmin {
    seen: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl ExtensionAdmin for MutatingExtensionAdmin {
    async fn status(&self) -> Result<ExtensionStatusResult, String> {
        Ok(ExtensionStatusResult::default())
    }

    async fn set_plugin_enabled(
        &self,
        actor: &str,
        plugin: &str,
        enabled: bool,
    ) -> Result<PluginRecord, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("enable:{actor}:{plugin}:{enabled}"));
        Ok(PluginRecord {
            name: plugin.to_owned(),
            version: "1.0.0".to_owned(),
            enabled,
            source_type: "local".to_owned(),
            source: "fixtures/demo".to_owned(),
            revision: String::new(),
            content_hash: "c".repeat(64),
            installed_at: "2026-09-20T00:00:00.000Z".to_owned(),
            required_capabilities: Vec::new(),
            resolved_dependencies: Vec::new(),
            diagnostics: Vec::new(),
            content_verified: true,
        })
    }

    async fn set_hook_review(
        &self,
        actor: &str,
        plugin: &str,
        hook: &str,
        trust_hash: &str,
        approved: bool,
    ) -> Result<HookRecord, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("review:{actor}:{plugin}:{hook}:{approved}"));
        Ok(HookRecord {
            plugin: plugin.to_owned(),
            id: hook.to_owned(),
            event: "PreToolUse".to_owned(),
            handler: "command:audit".to_owned(),
            order: 0,
            parallel: false,
            capabilities: vec!["execute".to_owned()],
            trust_hash: trust_hash.to_owned(),
            approved,
        })
    }
}

#[tokio::test]
async fn extension_mutations_are_actor_scoped_and_require_an_admin() {
    let directory = tempdir().unwrap();
    let server = scripted_server(Arc::new(ScriptedDriver::echo()), directory.path());

    // No admin configured -> the mutation fails closed (the read answers empty).
    let (client, task) = connected_client(server.clone()).await;
    let error = client
        .request(Command::ExtensionsPluginEnabled(PluginEnabledParams {
            idempotency_key: key("enable-no-admin"),
            plugin: "demo".to_owned(),
            enabled: true,
        }))
        .await
        .expect_err("a mutation without an admin must fail");
    let cool_app_server::ClientError::Protocol(error) = error else {
        panic!("expected a canonical protocol failure, got {error:?}");
    };
    assert_eq!(error.cool_code, "extension_admin_unavailable");
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    let admin = Arc::new(MutatingExtensionAdmin::default());
    let server = server.with_extension_admin(admin.clone());
    let (client, task) = connected_client(server.clone()).await;

    let payload = client
        .request(Command::ExtensionsPluginEnabled(PluginEnabledParams {
            idempotency_key: key("enable"),
            plugin: "demo".to_owned(),
            enabled: false,
        }))
        .await
        .unwrap();
    let ResponsePayload::ExtensionsPluginEnabled(record) = payload else {
        panic!("extensions.plugin_enabled must return ExtensionsPluginEnabled, got {payload:?}");
    };
    assert_eq!(record.name, "demo");
    assert!(!record.enabled);

    let payload = client
        .request(Command::ExtensionsHookReview(HookReviewParams {
            idempotency_key: key("review"),
            plugin: "demo".to_owned(),
            hook: "audit".to_owned(),
            trust_hash: "d".repeat(64),
            approved: true,
        }))
        .await
        .unwrap();
    let ResponsePayload::ExtensionsHookReviewed(hook) = payload else {
        panic!("extensions.hook_review must return ExtensionsHookReviewed, got {payload:?}");
    };
    assert_eq!(hook.id, "audit");
    assert!(hook.approved);
    assert_eq!(hook.trust_hash, "d".repeat(64));

    // A replay reaches the host again: the store mutation is naturally
    // idempotent, so repeating it is safe.
    client
        .request(Command::ExtensionsPluginEnabled(PluginEnabledParams {
            idempotency_key: key("enable-replay"),
            plugin: "demo".to_owned(),
            enabled: false,
        }))
        .await
        .unwrap();

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
    assert_eq!(
        admin.seen.lock().unwrap().as_slice(),
        [
            "enable:local-user:demo:false",
            "review:local-user:demo:audit:true",
            "enable:local-user:demo:false",
        ],
        "the server-derived actor must reach the host for every mutation"
    );

    // The read-only default for an admin that only supports reads fails closed
    // with a canonical mutation error rather than pretending success.
    let server = server.with_extension_admin(Arc::new(FailingExtensionAdmin));
    let (client, task) = connected_client(server).await;
    let error = client
        .request(Command::ExtensionsHookReview(HookReviewParams {
            idempotency_key: key("review-failing"),
            plugin: "demo".to_owned(),
            hook: "audit".to_owned(),
            trust_hash: "d".repeat(64),
            approved: true,
        }))
        .await
        .expect_err("a read-only admin must reject a mutation");
    let cool_app_server::ClientError::Protocol(error) = error else {
        panic!("expected a canonical protocol failure, got {error:?}");
    };
    assert_eq!(error.cool_code, "extension_mutation_failed");
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[derive(Default)]
struct RecordingMcpAdmin {
    seen: std::sync::Mutex<Vec<String>>,
}

fn admin_record(name: &str) -> McpServerAdminRecord {
    McpServerAdminRecord {
        name: name.to_owned(),
        transport: "stdio".to_owned(),
        status: "connected".to_owned(),
        enabled: true,
        description: String::new(),
        command: "echo".to_owned(),
        args: Vec::new(),
        url: String::new(),
        capabilities: Vec::new(),
        timeout_s: 30.0,
        version: String::new(),
        author: String::new(),
        compatibility: String::new(),
        error: None,
        tools: vec![McpToolRecord {
            name: "echo".to_owned(),
            qualified_name: format!("mcp_{name}_echo"),
            description: String::new(),
            server_name: name.to_owned(),
            input_schema: json!({"type": "object"}),
        }],
        server_info: None,
    }
}

#[async_trait]
impl McpAdmin for RecordingMcpAdmin {
    async fn list_servers(&self) -> Result<McpServerListResult, String> {
        Ok(McpServerListResult {
            servers: vec![admin_record("demo")],
        })
    }

    async fn add_server(
        &self,
        actor: &str,
        params: &McpAddServerParams,
    ) -> Result<McpServerAdminRecord, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("add:{actor}:{}:{}", params.name, params.transport));
        Ok(admin_record(&params.name))
    }

    async fn update_server(
        &self,
        actor: &str,
        params: &McpUpdateServerParams,
    ) -> Result<McpServerAdminRecord, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("update:{actor}:{}", params.name));
        Ok(admin_record(&params.name))
    }

    async fn remove_server(&self, actor: &str, name: &str) -> Result<(), String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("remove:{actor}:{name}"));
        Ok(())
    }

    async fn connect(&self, actor: &str, name: &str) -> Result<McpConnectResult, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("connect:{actor}:{name}"));
        Ok(McpConnectResult {
            name: name.to_owned(),
            status: "connected".to_owned(),
            tools_count: 1,
            error: None,
        })
    }

    async fn disconnect(&self, actor: &str, name: &str) -> Result<McpConnectResult, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("disconnect:{actor}:{name}"));
        Ok(McpConnectResult {
            name: name.to_owned(),
            status: "disconnected".to_owned(),
            tools_count: 0,
            error: None,
        })
    }

    async fn health(&self, name: &str) -> Result<McpHealthResult, String> {
        Ok(McpHealthResult {
            name: name.to_owned(),
            healthy: true,
        })
    }

    async fn list_tools(&self) -> Result<McpToolListResult, String> {
        Ok(McpToolListResult {
            tools: vec![admin_record("demo").tools[0].clone()],
        })
    }

    async fn reconnect_all(&self, actor: &str) -> Result<McpServerListResult, String> {
        self.seen.lock().unwrap().push(format!("reconnect:{actor}"));
        self.list_servers().await
    }
}

fn add_params(name: &str) -> McpAddServerParams {
    McpAddServerParams {
        idempotency_key: key("mcp-add"),
        name: name.to_owned(),
        transport: "stdio".to_owned(),
        command: "echo".to_owned(),
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        url: String::new(),
        headers: std::collections::BTreeMap::new(),
        enabled: true,
        description: String::new(),
        capabilities: Vec::new(),
        timeout_s: 30.0,
        version: String::new(),
        author: String::new(),
        compatibility: String::new(),
    }
}

#[tokio::test]
async fn mcp_admin_dispatch_is_actor_scoped_and_requires_an_admin() {
    let directory = tempdir().unwrap();
    let server = scripted_server(Arc::new(ScriptedDriver::echo()), directory.path());

    // No admin configured: a mutation fails closed and a read answers empty.
    let (client, task) = connected_client(server.clone()).await;
    let error = client
        .request(Command::McpAddServer(add_params("demo")))
        .await
        .expect_err("a mutation without an admin must fail");
    let cool_app_server::ClientError::Protocol(error) = error else {
        panic!("expected a canonical protocol failure, got {error:?}");
    };
    assert_eq!(error.cool_code, "mcp_admin_unavailable");
    let payload = client
        .request(Command::McpListServers(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::McpServersListed(list) = payload else {
        panic!("mcp.list_servers must return McpServersListed, got {payload:?}");
    };
    assert!(list.servers.is_empty());
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    let admin = Arc::new(RecordingMcpAdmin::default());
    let server = server.with_mcp_admin(admin.clone());
    let (client, task) = connected_client(server).await;

    let payload = client
        .request(Command::McpAddServer(add_params("demo")))
        .await
        .unwrap();
    let ResponsePayload::McpServerAdded(record) = payload else {
        panic!("mcp.add_server must return McpServerAdded, got {payload:?}");
    };
    assert_eq!(record.name, "demo");
    assert_eq!(record.tools[0].qualified_name, "mcp_demo_echo");

    let payload = client
        .request(Command::McpConnect(McpServerNameParams {
            name: "demo".to_owned(),
        }))
        .await
        .unwrap();
    let ResponsePayload::McpConnected(result) = payload else {
        panic!("mcp.connect must return McpConnected, got {payload:?}");
    };
    assert_eq!(result.status, "connected");
    assert_eq!(result.tools_count, 1);

    let payload = client
        .request(Command::McpHealth(McpServerNameParams {
            name: "demo".to_owned(),
        }))
        .await
        .unwrap();
    let ResponsePayload::McpHealth(result) = payload else {
        panic!("mcp.health must return McpHealth, got {payload:?}");
    };
    assert!(result.healthy);

    let payload = client
        .request(Command::McpListTools(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::McpToolsListed(tools) = payload else {
        panic!("mcp.list_tools must return McpToolsListed, got {payload:?}");
    };
    assert_eq!(tools.tools.len(), 1);

    let payload = client
        .request(Command::McpRemoveServer(McpServerNameParams {
            name: "demo".to_owned(),
        }))
        .await
        .unwrap();
    assert!(matches!(payload, ResponsePayload::McpServerRemoved(_)));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
    assert_eq!(
        admin.seen.lock().unwrap().as_slice(),
        [
            "add:local-user:demo:stdio",
            "connect:local-user:demo",
            "remove:local-user:demo",
        ],
        "the server-derived actor must reach the host for every mutation"
    );
}

#[derive(Default)]
struct RecordingSkillAdmin {
    seen: std::sync::Mutex<Vec<String>>,
}

fn skill_record(name: &str) -> SkillAdminRecord {
    SkillAdminRecord {
        name: name.to_owned(),
        description: String::new(),
        source: "user".to_owned(),
        tags: Vec::new(),
        tools: Vec::new(),
        version: "1.0".to_owned(),
        body: "body".to_owned(),
    }
}

#[async_trait]
impl SkillAdmin for RecordingSkillAdmin {
    async fn list(&self, source: Option<&str>) -> Result<SkillListResult, String> {
        if source == Some("plugin") {
            return Ok(SkillListResult::default());
        }
        Ok(SkillListResult {
            skills: vec![skill_record("demo")],
        })
    }

    async fn create(
        &self,
        actor: &str,
        params: &SkillCreateParams,
    ) -> Result<SkillCreateResult, String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("create:{actor}:{}", params.name));
        Ok(SkillCreateResult {
            name: params.name.clone(),
            path: format!("/skills/{}", params.name),
            scope: "user".to_owned(),
        })
    }

    async fn delete(&self, actor: &str, name: &str) -> Result<(), String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("delete:{actor}:{name}"));
        Ok(())
    }
}

#[tokio::test]
async fn skill_admin_dispatch_is_actor_scoped_and_requires_an_admin() {
    let directory = tempdir().unwrap();
    let server = scripted_server(Arc::new(ScriptedDriver::echo()), directory.path());

    // No admin configured: a mutation fails closed and a read answers empty.
    let (client, task) = connected_client(server.clone()).await;
    let error = client
        .request(Command::SkillsCreate(SkillCreateParams {
            idempotency_key: key("skill-create-no-admin"),
            name: "demo".to_owned(),
            description: String::new(),
            tags: Vec::new(),
            tools: Vec::new(),
            body: "body".to_owned(),
            scope: "user".to_owned(),
        }))
        .await
        .expect_err("a mutation without an admin must fail");
    let cool_app_server::ClientError::Protocol(error) = error else {
        panic!("expected a canonical protocol failure, got {error:?}");
    };
    assert_eq!(error.cool_code, "skills_admin_unavailable");
    let payload = client
        .request(Command::SkillsList(
            cool_protocol::SkillListParams::default(),
        ))
        .await
        .unwrap();
    let ResponsePayload::SkillsListed(list) = payload else {
        panic!("skills.list must return SkillsListed, got {payload:?}");
    };
    assert!(list.skills.is_empty());
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    let admin = Arc::new(RecordingSkillAdmin::default());
    let server = server.with_skill_admin(admin.clone());
    let (client, task) = connected_client(server).await;

    let payload = client
        .request(Command::SkillsCreate(SkillCreateParams {
            idempotency_key: key("skill-create"),
            name: "demo".to_owned(),
            description: String::new(),
            tags: Vec::new(),
            tools: Vec::new(),
            body: "body".to_owned(),
            scope: "user".to_owned(),
        }))
        .await
        .unwrap();
    let ResponsePayload::SkillCreated(created) = payload else {
        panic!("skills.create must return SkillCreated, got {payload:?}");
    };
    assert_eq!(created.name, "demo");
    assert_eq!(created.scope, "user");

    let payload = client
        .request(Command::SkillsList(
            cool_protocol::SkillListParams::default(),
        ))
        .await
        .unwrap();
    let ResponsePayload::SkillsListed(list) = payload else {
        panic!("skills.list must return SkillsListed, got {payload:?}");
    };
    assert_eq!(list.skills.len(), 1);

    let payload = client
        .request(Command::SkillsDelete(cool_protocol::SkillDeleteParams {
            idempotency_key: key("skill-delete"),
            name: "demo".to_owned(),
        }))
        .await
        .unwrap();
    assert!(matches!(payload, ResponsePayload::SkillDeleted(_)));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
    assert_eq!(
        admin.seen.lock().unwrap().as_slice(),
        ["create:local-user:demo", "delete:local-user:demo"],
        "the server-derived actor must reach the host for every mutation"
    );
}

#[tokio::test]
async fn extensions_status_reports_the_admin_snapshot_and_fails_closed() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();

    // No extension host configured -> an empty snapshot, not an error.
    let (client, task) = connected_client(server.clone()).await;
    let payload = client
        .request(Command::ExtensionsStatus(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::ExtensionsStatus(empty) = payload else {
        panic!("extensions.status must return ExtensionsStatus, got {payload:?}");
    };
    assert!(empty.plugins.is_empty());
    assert!(empty.workers.is_empty());
    assert!(empty.hooks.is_empty());
    assert!(empty.skills.is_empty());
    assert!(empty.mcp_servers.is_empty());
    assert!(empty.mcp_tool_policy.enabled.is_none());
    assert!(empty.mcp_tool_policy.disabled.is_empty());
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    let server = server.with_extension_admin(Arc::new(StaticExtensionAdmin));
    let (client, task) = connected_client(server.clone()).await;
    let payload = client
        .request(Command::ExtensionsStatus(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::ExtensionsStatus(snapshot) = payload else {
        panic!("extensions.status must return ExtensionsStatus, got {payload:?}");
    };
    assert_eq!(snapshot.plugins[0].name, "demo");
    assert!(snapshot.plugins[0].content_verified);
    assert_eq!(snapshot.plugins[0].required_capabilities, ["execute"]);
    assert_eq!(snapshot.workers[0].status, "failed");
    assert_eq!(snapshot.workers[0].attempt, 2);
    assert_eq!(snapshot.workers[0].code.as_deref(), Some("crash"));
    assert!(!snapshot.hooks[0].approved);
    assert_eq!(snapshot.hooks[0].trust_hash, "b".repeat(64));
    assert_eq!(snapshot.skills[0].name, "changelog");
    assert_eq!(snapshot.mcp_servers[0].transport, "stdio");
    assert_eq!(
        snapshot.mcp_tool_policy.enabled.as_deref(),
        Some(["read".to_owned()].as_slice())
    );
    assert_eq!(snapshot.mcp_tool_policy.disabled, ["write"]);
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    // A failing store read is a canonical failure, never a silent empty catalog.
    let server = server.with_extension_admin(Arc::new(FailingExtensionAdmin));
    let (client, task) = connected_client(server).await;
    let error = client
        .request(Command::ExtensionsStatus(EmptyParams {}))
        .await
        .expect_err("a failing extension host must fail the command");
    let cool_app_server::ClientError::Protocol(error) = error else {
        panic!("expected a canonical protocol failure, got {error:?}");
    };
    assert_eq!(error.cool_code, "extension_state_failed");
    assert_eq!(
        error.safe_details.get("detail"),
        Some(&serde_json::Value::String(
            "plugin store is invalid: boom".to_owned()
        ))
    );
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn tools_list_reports_the_runtime_catalog() {
    let directory = tempdir().unwrap();
    let server = scripted_server(Arc::new(ScriptedDriver::echo()), directory.path());
    let (client, task) = connected_client(server).await;
    let payload = client
        .request(Command::ToolsList(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::ToolsListed(catalog) = payload else {
        panic!("tools.list must return ToolsListed, got {payload:?}");
    };
    let names: Vec<&str> = catalog.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&"read_file"), "catalog: {names:?}");
    assert!(names.contains(&"shell"), "catalog: {names:?}");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(
        names, sorted,
        "catalog must be name-sorted and deterministic"
    );

    let read = catalog
        .iter()
        .find(|entry| entry.name == "read_file")
        .unwrap();
    assert!(!read.dangerous);
    assert!(read.capabilities.contains(&"read".to_owned()));
    assert!(!read.is_macro);

    let shell = catalog.iter().find(|entry| entry.name == "shell").unwrap();
    assert!(
        shell.dangerous,
        "approval-gated tools are flagged dangerous"
    );
    assert!(shell.capabilities.contains(&"execute".to_owned()));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn tasks_templates_reports_the_static_catalog() {
    let directory = tempdir().unwrap();
    let server = scripted_server(Arc::new(ScriptedDriver::echo()), directory.path());
    let (client, task) = connected_client(server).await;
    let payload = client
        .request(Command::TasksTemplates(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::TasksTemplatesListed(templates) = payload else {
        panic!("tasks.templates must return TasksTemplatesListed, got {payload:?}");
    };
    let slugs: Vec<&str> = templates
        .iter()
        .map(|template| template.slug.as_str())
        .collect();
    assert_eq!(
        slugs,
        [
            "news-digest",
            "code-review",
            "memory-review",
            "health-check"
        ]
    );
    let digest = templates.iter().find(|t| t.slug == "news-digest").unwrap();
    assert_eq!(digest.cron_expression, "0 8 * * *");
    assert_eq!(digest.max_iterations, 12);
    assert_eq!(digest.delivery_channels, ["ui"]);
    assert!(
        digest
            .tools_whitelist
            .as_ref()
            .is_some_and(|tools| tools.contains(&"web_search".to_owned()))
    );
    assert!(!digest.prompt.is_empty());

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn reconnect_catch_up_has_no_gaps_duplicates_or_repeated_side_effects() {
    let directory = tempdir().unwrap();
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let provider = Arc::new(ScriptedDriver::echo_with_delay(config.event_delay));
    let server = AppServer::with_agent_runtime(
        config,
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;
    let session_id = client
        .create_session("reconnect-session", None, None)
        .await
        .unwrap();
    let mut events = client.subscribe();
    let run_id = client
        .prompt("reconnect-prompt", &session_id, "hello", None)
        .await
        .unwrap()
        .run_id;

    // Read the first two events, then drop the client mid-run. The server
    // cancels the owned run on disconnect.
    let mut seen = Vec::new();
    for _ in 0..2 {
        let envelope = timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        if envelope.run_id == run_id {
            seen.push(envelope);
        }
    }
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
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
    .expect("disconnect cancellation becomes terminal");

    let (client, task) = connected_client(server.clone()).await;
    let retried = client
        .prompt("reconnect-prompt", &session_id, "hello", None)
        .await
        .unwrap();
    assert_eq!(retried.run_id, run_id, "idempotent retry reuses the run");
    assert_eq!(server.prompt_executions().await, 1);

    let mut pages = Vec::new();
    let mut after = None;
    loop {
        let page = client.run_events(&run_id, after, 2).await.unwrap();
        after = page
            .next_cursor
            .as_ref()
            .and_then(|cursor| cursor.after_seq);
        let has_more = page.has_more;
        pages.extend(page.events);
        if !has_more {
            break;
        }
    }
    let sequences = pages.iter().map(|event| event.seq).collect::<Vec<_>>();
    assert_eq!(sequences, (1..=sequences.len() as u64).collect::<Vec<_>>());
    let unique = pages
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), pages.len());
    let caught_up = seen
        .iter()
        .all(|event| pages.iter().any(|page| page.event_id == event.event_id));
    assert!(caught_up, "every observed event is in the durable log");
    assert!(matches!(
        pages.last().unwrap().event,
        CanonicalEvent::RunCancelled(_)
    ));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn oversized_history_is_bounded_by_the_negotiated_frame_limit() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = AppServer::with_agent_runtime(
        ServerConfig {
            max_frame_bytes: 1_200,
            ..ServerConfig::default()
        },
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, builtin_registry()),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (client, task) = connected_client(server.clone()).await;

    // The transport frame guard rejects oversized live events, so a store-level
    // append proves the read path also stays bounded.
    let append = |session: &str, run: &str, content: &str| {
        let envelope = cool_protocol::EventEnvelope {
            event_id: format!("event-{run}-{}", content.len()),
            schema_version: cool_protocol::V1Version::VALUE,
            session_id: session.to_owned(),
            run_id: run.to_owned(),
            item_id: None,
            seq: 0,
            occurred_at: "2026-09-17T00:00:00.000Z".to_owned(),
            actor: cool_protocol::ActorRef {
                id: "local-user".to_owned(),
                kind: cool_protocol::ActorKind::LocalUser,
            },
            source: "test".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::ItemCompleted(cool_protocol::ItemEvent {
                role: Some("user".to_owned()),
                content: Some(content.to_owned()),
                tool_calls: Vec::new(),
            }),
            extensions: Default::default(),
        };
        server
            .store()
            .append_event_auto("local-user", envelope)
            .unwrap();
    };

    let huge = server
        .store()
        .create_session("local-user", "huge-session", "huge-session", None, None)
        .unwrap()
        .value;
    let huge_run = server
        .store()
        .start_run("local-user", "huge-run", "huge-run", &huge)
        .unwrap()
        .value;
    append(&huge, &huge_run, &"x".repeat(4_000));
    assert!(matches!(
        client.session_history(&huge, 50).await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "outbound_frame_too_large"
    ));

    let mixed = server
        .store()
        .create_session("local-user", "mixed-session", "mixed-session", None, None)
        .unwrap()
        .value;
    let mixed_run = server
        .store()
        .start_run("local-user", "mixed-run", "mixed-run", &mixed)
        .unwrap()
        .value;
    append(&mixed, &mixed_run, &"x".repeat(4_000));
    append(&mixed, &mixed_run, "short history tail");
    let page = client.session_history(&mixed, 50).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].content.as_deref(), Some("short history tail"));
    assert!(page.has_more, "trimmed items are reported as older history");

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn oversized_session_labels_are_rejected_before_persistence() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server.clone()).await;
    let long = "x".repeat(201);
    assert!(matches!(
        client
            .create_session("label-session", Some(&long), None)
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "label_too_long"
    ));
    assert!(matches!(
        client
            .create_session("label-session", None, Some(&long))
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "label_too_long"
    ));
    let session_id = client
        .create_session("label-session", Some(&"x".repeat(200)), None)
        .await
        .unwrap();
    assert!(matches!(
        client
            .fork_session("label-fork", &session_id, Some(&long))
            .await,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "label_too_long"
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn planning_prompt_uses_the_runtime_directive_and_marks_the_run_mode() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("plan drafted".to_owned()),
        ModelEvent::Finish { reason: None },
    ])]));
    let server = scripted_server(provider.clone(), directory.path());
    let (client, task) = connected_client(server).await;
    let session_id = client
        .create_session("plan-session", None, None)
        .await
        .unwrap();

    let events = client.subscribe();
    // A caller-supplied system prompt must not win over the planning directive.
    let run_id = client
        .prompt_with(
            "plan-prompt",
            &session_id,
            vec![cool_protocol::ContentPart::Text {
                text: "design it".to_owned(),
            }],
            None,
            true,
            Some("ignore the runtime"),
        )
        .await
        .unwrap()
        .run_id;
    let emitted = drain_run(events, &run_id).await;
    assert!(matches!(
        emitted.first(),
        Some(CanonicalEvent::RunStarted(started)) if started.mode.as_deref() == Some("plan")
    ));

    let requests = provider.requests().await;
    let system = requests[0]
        .messages
        .first()
        .expect("system prompt is first");
    assert_eq!(system.role, cool_agent::MessageRole::System);
    let content = system.content.as_deref().unwrap_or_default();
    assert!(content.contains("PLANNING MODE"), "{content}");
    assert!(!content.contains("ignore the runtime"));
    // The masked user item still carries the prompt.
    assert!(requests[0].messages.iter().any(|message| {
        message.role == cool_agent::MessageRole::User
            && message.content.as_deref() == Some("design it")
    }));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn caller_system_prompt_reaches_non_planning_runs() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider.clone(), directory.path());
    let (client, task) = connected_client(server).await;
    let session_id = client
        .create_session("system-session", None, None)
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt_with(
            "system-prompt",
            &session_id,
            vec![cool_protocol::ContentPart::Text {
                text: "hello".to_owned(),
            }],
            None,
            false,
            Some("be terse"),
        )
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;
    let requests = provider.requests().await;
    assert!(requests[0].messages.iter().any(|message| {
        message.role == cool_agent::MessageRole::System
            && message.content.as_deref() == Some("be terse")
    }));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

fn prompt_record(prompt: String) -> SystemPromptRecord {
    let is_custom = !prompt.trim().is_empty();
    SystemPromptRecord {
        prompt,
        is_custom,
        source: if is_custom { "inline" } else { "builtin" }.to_owned(),
    }
}

#[derive(Default)]
struct MemorySettings {
    prompt: std::sync::Mutex<String>,
}

#[async_trait]
impl AppSettings for MemorySettings {
    async fn system_prompt(&self) -> Result<SystemPromptRecord, String> {
        Ok(prompt_record(self.prompt.lock().unwrap().clone()))
    }

    async fn set_system_prompt(&self, prompt: &str) -> Result<SystemPromptRecord, String> {
        let stored = if prompt.trim().is_empty() {
            String::new()
        } else {
            prompt.to_owned()
        };
        *self.prompt.lock().unwrap() = stored.clone();
        Ok(prompt_record(stored))
    }
}

#[tokio::test]
async fn settings_system_prompt_defaults_empty_and_round_trips_with_a_host() {
    let directory = tempdir().unwrap();
    let server = scripted_server(Arc::new(ScriptedDriver::echo()), directory.path());

    // No settings host -> a built-in (empty) default; a write fails closed.
    let (client, task) = connected_client(server.clone()).await;
    let payload = client
        .request(Command::SettingsSystemPrompt(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::SettingsSystemPrompt(record) = payload else {
        panic!("settings.system_prompt must return SettingsSystemPrompt, got {payload:?}");
    };
    assert_eq!(record.prompt, "");
    assert!(!record.is_custom);
    assert_eq!(record.source, "builtin");
    let error = client
        .request(Command::SettingsSystemPromptSet(SystemPromptSetParams {
            idempotency_key: key("settings-no-host"),
            prompt: "hello".to_owned(),
        }))
        .await
        .expect_err("a write without a settings host must fail");
    let cool_app_server::ClientError::Protocol(error) = error else {
        panic!("expected a canonical protocol failure, got {error:?}");
    };
    assert_eq!(error.cool_code, "settings_unavailable");
    drop(client);
    task.await.expect("server task").expect("clean disconnect");

    // With a host the value round-trips; a whitespace-only value clears it.
    let server = server.with_app_settings(Arc::new(MemorySettings::default()));
    let (client, task) = connected_client(server).await;
    let payload = client
        .request(Command::SettingsSystemPromptSet(SystemPromptSetParams {
            idempotency_key: key("settings-set"),
            prompt: "You are terse.".to_owned(),
        }))
        .await
        .unwrap();
    let ResponsePayload::SettingsSystemPrompt(record) = payload else {
        panic!("settings.system_prompt_set must return SettingsSystemPrompt, got {payload:?}");
    };
    assert_eq!(record.prompt, "You are terse.");
    assert!(record.is_custom);
    assert_eq!(record.source, "inline");

    let payload = client
        .request(Command::SettingsSystemPrompt(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::SettingsSystemPrompt(record) = payload else {
        panic!("settings.system_prompt must return SettingsSystemPrompt, got {payload:?}");
    };
    assert_eq!(record.prompt, "You are terse.");

    client
        .request(Command::SettingsSystemPromptSet(SystemPromptSetParams {
            idempotency_key: key("settings-clear"),
            prompt: "   ".to_owned(),
        }))
        .await
        .unwrap();
    let payload = client
        .request(Command::SettingsSystemPrompt(EmptyParams {}))
        .await
        .unwrap();
    let ResponsePayload::SettingsSystemPrompt(record) = payload else {
        panic!("settings.system_prompt must return SettingsSystemPrompt, got {payload:?}");
    };
    assert!(record.prompt.is_empty());
    assert!(!record.is_custom);
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn persisted_default_system_prompt_reaches_a_normal_turn() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let settings = Arc::new(MemorySettings::default());
    *settings.prompt.lock().unwrap() = "be brief".to_owned();
    let server = scripted_server(provider.clone(), directory.path()).with_app_settings(settings);
    let (client, task) = connected_client(server).await;
    let session_id = client
        .create_session("settings-turn", None, None)
        .await
        .unwrap();

    // A normal turn with no caller prompt uses the persisted default.
    let events = client.subscribe();
    let run_id = client
        .prompt_with(
            "settings-default",
            &session_id,
            vec![cool_protocol::ContentPart::Text {
                text: "hello".to_owned(),
            }],
            None,
            false,
            None,
        )
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;

    // A caller prompt still wins over the persisted default.
    let events = client.subscribe();
    let run_id = client
        .prompt_with(
            "settings-override",
            &session_id,
            vec![cool_protocol::ContentPart::Text {
                text: "again".to_owned(),
            }],
            None,
            false,
            Some("be terse"),
        )
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;

    let requests = provider.requests().await;
    let system = |index: usize| -> Option<String> {
        requests[index]
            .messages
            .iter()
            .find(|message| message.role == cool_agent::MessageRole::System)
            .and_then(|message| message.content.clone())
    };
    assert_eq!(system(0).as_deref(), Some("be brief"));
    assert_eq!(system(1).as_deref(), Some("be terse"));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

struct FailingSettings;

#[async_trait]
impl AppSettings for FailingSettings {
    async fn system_prompt(&self) -> Result<SystemPromptRecord, String> {
        Err("settings file is invalid: boom".to_owned())
    }

    async fn set_system_prompt(&self, _prompt: &str) -> Result<SystemPromptRecord, String> {
        Err("settings file is invalid: boom".to_owned())
    }
}

#[tokio::test]
async fn plan_mode_ignores_the_persisted_default_prompt() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("plan drafted".to_owned()),
        ModelEvent::Finish { reason: None },
    ])]));
    let settings = Arc::new(MemorySettings::default());
    *settings.prompt.lock().unwrap() = "be brief".to_owned();
    let server = scripted_server(provider.clone(), directory.path()).with_app_settings(settings);
    let (client, task) = connected_client(server).await;
    let session_id = client
        .create_session("plan-settings", None, None)
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt_with(
            "plan-default",
            &session_id,
            vec![cool_protocol::ContentPart::Text {
                text: "design it".to_owned(),
            }],
            None,
            true,
            None,
        )
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;
    let requests = provider.requests().await;
    let system = requests[0]
        .messages
        .first()
        .and_then(|message| message.content.clone())
        .unwrap_or_default();
    assert!(system.contains("PLANNING MODE"), "{system}");
    assert!(!system.contains("be brief"), "plan mode owns the prompt");
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn a_settings_read_failure_does_not_fail_a_turn() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider.clone(), directory.path())
        .with_app_settings(Arc::new(FailingSettings));
    let (client, task) = connected_client(server).await;
    let session_id = client
        .create_session("failing-settings", None, None)
        .await
        .unwrap();
    let events = client.subscribe();
    let run_id = client
        .prompt_with(
            "failing-settings-prompt",
            &session_id,
            vec![cool_protocol::ContentPart::Text {
                text: "hello".to_owned(),
            }],
            None,
            false,
            None,
        )
        .await
        .unwrap()
        .run_id;
    // The turn still completes; a settings read failure only drops the default.
    let emitted = drain_run(events, &run_id).await;
    assert!(
        emitted
            .iter()
            .any(|event| matches!(event, CanonicalEvent::RunCompleted(_))),
        "the run must complete despite the settings read failure"
    );
    let requests = provider.requests().await;
    assert!(
        requests[0]
            .messages
            .iter()
            .all(|message| message.role != cool_agent::MessageRole::System),
        "a failed settings read must not inject a system message"
    );
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn session_history_paginates_on_a_durable_cursor_without_splitting_reasoning() {
    let directory = tempdir().unwrap();
    // Each assistant turn streams several reasoning deltas then content, so a
    // page boundary can fall inside a group. The deltas must travel with the
    // assistant item they belong to, never split onto a different page.
    let turn = |prefix: &str| {
        Ok(vec![
            ModelEvent::Reasoning(format!("{prefix} a")),
            ModelEvent::Reasoning(format!("{prefix} b")),
            ModelEvent::Reasoning(format!("{prefix} c")),
            ModelEvent::Content(prefix.to_owned()),
            ModelEvent::Usage(cool_agent::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost_micro_usd: Some(1_000),
            }),
            ModelEvent::Finish { reason: None },
        ])
    };
    let provider = Arc::new(ScriptedDriver::new([
        turn("first"),
        turn("second"),
        turn("third"),
    ]));
    let server = scripted_server(provider, directory.path());
    let (client, task) = connected_client(server).await;
    let session_id = client
        .create_session("page-session", None, None)
        .await
        .unwrap();
    for key in ["page-1", "page-2", "page-3"] {
        let events = client.subscribe();
        let run_id = client
            .prompt(key, &session_id, key, None)
            .await
            .unwrap()
            .run_id;
        drain_run(events, &run_id).await;
    }

    // An asymmetric limit makes the boundary land mid-group for at least one
    // page (three items per turn: user + assistant).
    let mut cursor = None;
    let mut pages = Vec::new();
    loop {
        let page = client
            .session_history_page(&session_id, 3, cursor)
            .await
            .unwrap();
        assert!(page.items.len() <= 3);
        pages.push(page.clone());
        if !page.has_more {
            assert!(page.next_cursor.is_none(), "final page has no cursor");
            break;
        }
        let next = page.next_cursor.expect("a non-final page exposes a cursor");
        assert!(cursor.is_none_or(|previous| next < previous));
        cursor = Some(next);
    }
    // Pages arrive newest-first; within a page items stay chronological.
    // Reverse the page order to walk the transcript oldest-first.
    let items = pages
        .iter()
        .rev()
        .flat_map(|page| page.items.iter())
        .collect::<Vec<_>>();
    let roles = items
        .iter()
        .map(|item| item.role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        roles,
        [
            "user",
            "assistant",
            "user",
            "assistant",
            "user",
            "assistant"
        ]
    );
    // No item may be duplicated or dropped across page boundaries.
    let contents = items
        .iter()
        .filter_map(|item| item.content.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        contents,
        ["page-1", "first", "page-2", "second", "page-3", "third"]
    );
    for item in &items {
        if item.role == "assistant" {
            let prefix = item.content.as_deref().unwrap();
            assert_eq!(
                item.reasoning.as_deref(),
                Some(format!("{prefix} a{prefix} b{prefix} c").as_str()),
                "reasoning for {prefix} was split or lost"
            );
        }
    }
    // The enriched projection carries a stable cursor, run id and RFC3339
    // timestamp per item, and the model/usage that produced each assistant
    // turn, so the React transcript can render persisted history.
    let mut cursors = std::collections::HashSet::new();
    for item in &items {
        assert!(item.cursor > 0, "cursor is the durable rowid");
        assert!(cursors.insert(item.cursor), "cursors are unique");
        assert!(
            item.occurred_at.contains('T') && item.occurred_at.ends_with('Z'),
            "occurred_at is RFC3339: {}",
            item.occurred_at
        );
        assert!(!item.run_id.is_empty(), "item carries its run id");
        if item.role == "assistant" {
            assert_eq!(item.model.as_deref(), Some("scripted"));
            let usage = item.usage.as_ref().expect("assistant usage is attached");
            assert_eq!((usage.prompt_tokens, usage.completion_tokens), (10, 5));
            assert_eq!(usage.cost_usd, Some(0.001));
        } else {
            assert!(item.usage.is_none(), "only assistant items carry usage");
        }
    }
    assert!(pages[0].has_more);

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn run_subscribe_fans_live_events_out_to_a_second_connection() {
    let directory = tempdir().unwrap();
    // A gated tool holds the run live deterministically while the second
    // connection subscribes, so this exercises the fan-out path rather than
    // catch-up.
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Content("before".to_owned()),
            ModelEvent::ToolCall(ToolCall {
                call_id: "call-gate".to_owned(),
                name: "gate_tool".to_owned(),
                arguments: Default::default(),
            }),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("after".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]));
    let mut registry = builtin_registry();
    registry = registry
        .extend([Tool::new(
            ToolDefinition {
                name: "gate_tool".to_owned(),
                description: "deterministic gate".to_owned(),
                parameters: json!({"type": "object"}),
            },
            [],
            Decision::Allow,
            GateTool {
                started: started.clone(),
                release: release.clone(),
            },
        )])
        .unwrap();
    let server = AppServer::with_agent_runtime(
        ServerConfig::default(),
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(provider, registry),
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap();
    let (owner, owner_task) = connected_client(server.clone()).await;
    let session_id = owner
        .create_session("fanout-session", None, None)
        .await
        .unwrap();
    let run_id = owner
        .prompt("fanout-prompt", &session_id, "hello", None)
        .await
        .unwrap()
        .run_id;

    // Wait until the run is parked inside the tool, so it is guaranteed live.
    timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("run reaches the gate tool");

    // A different connection subscribes to the live run. Its subscription
    // returns a durable cursor and then receives live events; the local
    // receiver is created first so no fan-out event is dropped.
    let (subscriber, subscriber_task) = connected_client(server.clone()).await;
    let mut live = subscriber.subscribe();
    let subscription = subscriber.run_subscribe(&run_id).await.unwrap();
    assert_eq!(subscription.run_id, run_id);
    assert!(!subscription.terminal);

    // Release the tool so the run emits its remaining events and terminates.
    release.notify_one();
    let mut received = Vec::new();
    timeout(Duration::from_secs(5), async {
        while let Ok(envelope) = live.recv().await {
            if envelope.run_id != run_id {
                continue;
            }
            let terminal = matches!(
                envelope.event,
                CanonicalEvent::RunCompleted(_)
                    | CanonicalEvent::RunFailed(_)
                    | CanonicalEvent::RunCancelled(_)
            );
            received.push(envelope.event);
            if terminal {
                break;
            }
        }
    })
    .await
    .expect("subscriber receives the run's events");

    assert!(
        received.iter().any(
            |event| matches!(event, CanonicalEvent::ContentDelta(delta) if delta.text == "after")
        ),
        "live post-subscription content must be fanned out: {received:?}"
    );
    assert!(matches!(
        received.last(),
        Some(CanonicalEvent::RunCompleted(_))
    ));
    // Catch-up from the subscription cursor reaches the same durable events.
    let page = subscriber.run_events(&run_id, None, 50).await.unwrap();
    assert!(
        page.events
            .iter()
            .any(|event| matches!(event.event, CanonicalEvent::RunCompleted(_)))
    );

    drop(owner);
    drop(subscriber);
    owner_task
        .await
        .expect("owner task")
        .expect("clean disconnect");
    subscriber_task
        .await
        .expect("subscriber task")
        .expect("clean disconnect");
}

#[tokio::test]
async fn run_subscribe_receives_the_terminal_when_the_owner_disconnects() {
    let directory = tempdir().unwrap();
    // Long delay: the owner disconnects mid-run, so the disconnect cancellation
    // path (not the run sink) records the terminal event.
    let provider = Arc::new(ScriptedDriver::echo_with_delay(Duration::from_secs(10)));
    let server = scripted_server(provider, directory.path());
    let (owner, owner_task) = connected_client(server.clone()).await;
    let session_id = owner
        .create_session("disconnect-sub", None, None)
        .await
        .unwrap();
    let run_id = owner
        .prompt("disconnect-sub-prompt", &session_id, "hi", None)
        .await
        .unwrap()
        .run_id;

    let (subscriber, subscriber_task) = connected_client(server.clone()).await;
    let mut live = subscriber.subscribe();
    let subscription = subscriber.run_subscribe(&run_id).await.unwrap();
    assert!(!subscription.terminal);

    // Drop the owner; the server cancels its owned run and the terminal event
    // must reach the subscriber.
    drop(owner);
    owner_task
        .await
        .expect("owner task")
        .expect("clean disconnect");
    let terminal = timeout(Duration::from_secs(5), async {
        loop {
            let envelope = live.recv().await.expect("event channel open");
            if envelope.run_id == run_id
                && matches!(envelope.event, CanonicalEvent::RunCancelled(_))
            {
                return envelope.event;
            }
        }
    })
    .await
    .expect("subscriber observes the terminal cancellation");
    assert!(matches!(terminal, CanonicalEvent::RunCancelled(_)));

    drop(subscriber);
    subscriber_task
        .await
        .expect("subscriber task")
        .expect("clean disconnect");
}

#[tokio::test]
async fn run_subscribe_reports_a_terminal_run_without_waiting() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider, directory.path());
    let (owner, owner_task) = connected_client(server.clone()).await;
    let session_id = owner
        .create_session("terminal-sub", None, None)
        .await
        .unwrap();
    let events = owner.subscribe();
    let run_id = owner
        .prompt("terminal-sub-prompt", &session_id, "hi", None)
        .await
        .unwrap()
        .run_id;
    drain_run(events, &run_id).await;

    let (subscriber, subscriber_task) = connected_client(server).await;
    let subscription = subscriber.run_subscribe(&run_id).await.unwrap();
    assert!(
        subscription.terminal,
        "terminal run is reported as terminal"
    );
    drop(owner);
    drop(subscriber);
    owner_task
        .await
        .expect("owner task")
        .expect("clean disconnect");
    subscriber_task
        .await
        .expect("subscriber task")
        .expect("clean disconnect");
}

#[tokio::test]
async fn client_requests_fail_closed_before_initialize() {
    let directory = tempdir().unwrap();
    let provider = Arc::new(ScriptedDriver::echo());
    let server = scripted_server(provider, directory.path());
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).unwrap();
    let result = client.list_sessions(None, 10).await;
    assert!(matches!(
        result,
        Err(cool_app_server::ClientError::Protocol(error))
            if error.cool_code == "not_initialized"
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}
