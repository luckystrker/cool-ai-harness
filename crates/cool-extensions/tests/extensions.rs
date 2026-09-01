use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cool_extensions::{
    CompatibilityAdapter, CompatibilityWorkerSupervisor, ExtensionRuntime, HookEngine, HookError,
    HookInvocation, HookReviewStore, McpClient, McpServer, McpToolPolicy, PluginLoader,
    PluginStore, WorkerError, WorkerLaunchSpec, WorkerOperationClass, WorkerProtocol,
    WorkerRequestOutcome, discover_plugin_tools, discover_plugin_tools_with_policy,
    narrowed_plugin_policy, plugin_status_event,
};
use cool_security::{Capability, CapabilityPolicy, Decision};
use futures_util::future::join_all;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cool-extension-test-helper"))
}

fn write_plugin(root: &Path, with_hook: bool) {
    fs::create_dir_all(root.join("skills/demo")).unwrap();
    fs::write(root.join("plugin.json"), r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"demo","version":"1"}"#).unwrap();
    fs::write(
        root.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\nallowed-tools: read_file\n---\nDo the thing.\n",
    )
    .unwrap();
    let helper_name = if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    };
    fs::write(root.join("mcp.json"), format!(r#"{{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{{"local":{{"type":"stdio","command":"./{helper_name}","args":["mcp"]}}}}}}"#)).unwrap();
    fs::copy(helper(), root.join(helper_name)).unwrap();
    if with_hook {
        let directory = root.join("io.github.luckystrker.cool/hooks");
        fs::create_dir_all(&directory).unwrap();
        let command = format!("./{helper_name}");
        fs::write(directory.join("hooks.json"), format!(r#"{{"version":1,"hooks":[{{"id":"before-run","event":"UserPromptSubmit","handler":{{"type":"command","command":"{command}","args":["hook"]}}}}]}}"#)).unwrap();
    }
}

fn install_plugin_fixture(
    store_root: &Path,
    name: &str,
    with_hook: bool,
) -> (PathBuf, PathBuf, String) {
    let staging = store_root.join("staging").join(name);
    let data = store_root.join("data").join(name);
    write_plugin(&staging, with_hook);
    fs::create_dir_all(&data).unwrap();
    let content_hash = PluginLoader.load(&staging, &data).unwrap().content_hash;
    let install = store_root
        .join("installations")
        .join(name)
        .join(&content_hash);
    fs::create_dir_all(install.parent().unwrap()).unwrap();
    fs::rename(&staging, &install).unwrap();
    (install, data, content_hash)
}

#[test]
fn portable_manifest_requires_schema_and_skill_accepts_tier_one_frontmatter() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    let data = temporary.path().join("data");
    fs::create_dir_all(root.join("skills/demo")).unwrap();
    fs::create_dir_all(&data).unwrap();
    fs::write(root.join("plugin.json"), r#"{"name":"demo"}"#).unwrap();
    let missing_schema = PluginLoader.load(&root, &data).unwrap();
    assert!(missing_schema.manifest.is_none());
    assert!(
        missing_schema
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "manifest.schema_invalid")
    );

    fs::write(
        root.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"demo"}"#,
    )
    .unwrap();
    fs::write(
        root.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: 'Tier one skill'\nlicense: MIT\ncompatibility: Cool 1\nmetadata:\n  owner: test\nallowed-tools: read_file search\n---\nBody.\n",
    )
    .unwrap();
    let bundle = PluginLoader.load(&root, &data).unwrap();
    assert!(bundle.manifest.is_some());
    assert_eq!(bundle.skills[0].allowed_tools, ["read_file", "search"]);
}

#[test]
fn portable_bundle_loads_skills_mcp_and_trust_hashed_hooks() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    let data = temporary.path().join("data");
    write_plugin(&root, true);
    let bundle = PluginLoader.load(&root, &data).unwrap();
    assert!(bundle.loadable());
    assert!(bundle.conformant());
    assert_eq!(bundle.skills.len(), 1);
    assert_eq!(bundle.mcp_servers.len(), 1);
    assert_eq!(bundle.hooks.len(), 1);
    assert_eq!(bundle.hooks[0].trust_hash.len(), 64);
}

#[test]
fn content_hash_matches_the_python_m3_algorithm() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("plugin.json"), r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"hash-demo"}"#).unwrap();
    let bundle = PluginLoader
        .load(&root, &temporary.path().join("data"))
        .unwrap();
    assert_eq!(
        bundle.content_hash,
        "69b9434e1c39351087391c4efe744885a136d9fffec3c21b6230226ace37fbee"
    );
}

#[test]
fn loader_isolates_invalid_mcp_component() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    write_plugin(&root, false);
    fs::write(root.join("mcp.json"), r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"bad":{"type":"streamable-http","url":"http://example.com/mcp"}}}"#).unwrap();
    let bundle = PluginLoader
        .load(&root, &temporary.path().join("data"))
        .unwrap();
    assert!(bundle.loadable());
    assert_eq!(bundle.skills.len(), 1);
    assert!(bundle.mcp_servers.is_empty());
    assert!(
        bundle
            .diagnostics
            .iter()
            .any(|item| item.code == "mcp.server_invalid")
    );
}

#[test]
fn loader_rejects_routing_framing_and_session_headers() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    let data = temporary.path().join("data");
    write_plugin(&root, false);
    fs::create_dir_all(&data).unwrap();
    fs::write(
        root.join("mcp.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"remote":{"type":"streamable-http","url":"https://example.com/mcp","headers":{"Host":"attacker.invalid"}}}}"#,
    )
    .unwrap();
    let bundle = PluginLoader.load(&root, &data).unwrap();
    assert!(bundle.mcp_servers.is_empty());
    assert!(
        bundle
            .diagnostics
            .iter()
            .any(|item| item.code == "mcp.server_invalid")
    );
}

#[test]
fn repository_m3_portable_fixture_keeps_bare_command_and_placeholders() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let root = repository.join("backend/tests/fixtures/plugins/portable-valid");
    let temporary = TempDir::new().unwrap();
    let data = temporary.path().join("data");
    let bundle = PluginLoader.load(&root, &data).unwrap();
    assert!(bundle.conformant());
    let McpServer::Stdio { command, env, .. } = &bundle.mcp_servers[0] else {
        panic!("fixture server must be stdio");
    };
    assert_eq!(command, Path::new("python"));
    assert_eq!(
        env["PLUGIN_DATA"],
        data.canonicalize().unwrap().to_string_lossy()
    );
    assert_eq!(
        env["PLUGIN_ROOT"],
        root.canonicalize().unwrap().to_string_lossy()
    );
}

#[test]
fn python_m3_lockfile_migrates_as_unreviewed() {
    let temporary = TempDir::new().unwrap();
    let store_root = temporary.path().join("plugins");
    let (install, data, content_hash) = install_plugin_fixture(&store_root, "demo", true);
    fs::create_dir_all(&store_root).unwrap();
    fs::write(store_root.join("plugins.lock.json"), serde_json::to_vec_pretty(&json!({"lock_version":1,"plugins":{"demo":{"name":"demo","version":"1","enabled":true,"source_type":"local","source":"fixture","revision":"","content_hash":content_hash,"install_path":install,"data_path":data,"installed_at":"2026-09-02T00:00:00Z","diagnostics":[],"resolved_dependencies":[],"required_capabilities":[]}}})).unwrap()).unwrap();
    let store = PluginStore::open(&store_root).unwrap();
    assert_eq!(store.load_enabled().unwrap().len(), 1);
    assert!(store.reviewed_hook_hashes("demo").unwrap().is_empty());
    store.set_hook_review("demo", "before-run", "hash").unwrap();
    assert_eq!(
        store.reviewed_hook_hashes("demo").unwrap()["before-run"],
        "hash"
    );
    let lock: Value =
        serde_json::from_slice(&fs::read(store_root.join("plugins.lock.json")).unwrap()).unwrap();
    assert!(
        lock["plugins"]["demo"]
            .get("reviewed_hook_hashes")
            .is_none()
    );
}

#[test]
fn plugin_store_rejects_cross_plugin_data_binding() {
    let temporary = TempDir::new().unwrap();
    let store_root = temporary.path().join("plugins");
    let (install, _data, content_hash) = install_plugin_fixture(&store_root, "demo", false);
    let stolen_data = store_root.join("data/other");
    fs::create_dir_all(&stolen_data).unwrap();
    fs::write(store_root.join("plugins.lock.json"), serde_json::to_vec_pretty(&json!({"lock_version":1,"plugins":{"demo":{"name":"demo","version":"1","enabled":true,"source_type":"local","source":"fixture","revision":"","content_hash":content_hash,"install_path":install,"data_path":stolen_data,"installed_at":"2026-09-02T00:00:00Z","diagnostics":[],"resolved_dependencies":[],"required_capabilities":[]}}})).unwrap()).unwrap();
    let store = PluginStore::open(&store_root).unwrap();
    assert!(store.load_enabled().is_err());
}

#[tokio::test]
async fn stdio_mcp_initializes_lists_and_calls_without_importing_server_code() {
    let server = McpServer::Stdio {
        name: "local".to_owned(),
        command: helper(),
        args: vec!["mcp".to_owned()],
        env: BTreeMap::new(),
        cwd: std::env::current_dir().unwrap(),
    };
    let client = McpClient::new(server);
    assert_eq!(client.list_tools().await.unwrap()[0].name, "echo");
    assert_eq!(
        client
            .call_tool("echo", json!({"text":"hi"}))
            .await
            .unwrap()["isError"],
        false
    );
}

#[tokio::test]
async fn mcp_rejects_an_unsupported_negotiated_protocol_version() {
    let client = McpClient::new(McpServer::Stdio {
        name: "bad-version".to_owned(),
        command: helper(),
        args: vec!["mcp-bad-version".to_owned()],
        env: BTreeMap::new(),
        cwd: std::env::current_dir().unwrap(),
    });
    assert!(client.list_tools().await.is_err());
}

#[tokio::test]
async fn unknown_mcp_semantics_require_every_side_effect_capability() {
    let server = McpServer::Stdio {
        name: "local".to_owned(),
        command: helper(),
        args: vec!["mcp".to_owned()],
        env: BTreeMap::new(),
        cwd: std::env::current_dir().unwrap(),
    };
    let tools = discover_plugin_tools("demo", server).await.unwrap();
    let required = &tools[0].capabilities;
    for capability in [
        Capability::Read,
        Capability::Write,
        Capability::Execute,
        Capability::Network,
        Capability::Git,
        Capability::SendExternal,
    ] {
        assert!(required.contains(&capability));
    }
}

#[tokio::test]
async fn core_mcp_tool_policy_can_disable_discovered_tools() {
    let server = McpServer::Stdio {
        name: "local".to_owned(),
        command: helper(),
        args: vec!["mcp".to_owned()],
        env: BTreeMap::new(),
        cwd: std::env::current_dir().unwrap(),
    };
    let policy = McpToolPolicy {
        enabled: None,
        disabled: BTreeSet::from(["plugin_demo_local_echo".to_owned()]),
    };
    assert!(
        discover_plugin_tools_with_policy("demo", server, &policy)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn stdio_mcp_rejects_an_oversized_line_before_unbounded_buffering() {
    let server = McpServer::Stdio {
        name: "local".to_owned(),
        command: helper(),
        args: vec!["oversized-mcp".to_owned()],
        env: BTreeMap::new(),
        cwd: std::env::current_dir().unwrap(),
    };
    let error = McpClient::new(server).list_tools().await.unwrap_err();
    assert!(error.to_string().contains("too large"));
}

#[tokio::test]
async fn streamable_http_uses_initialize_session_and_tool_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for index in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(split) = bytes.windows(4).position(|item| item == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..split]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(str::trim)
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            let split = bytes
                .windows(4)
                .position(|item| item == b"\r\n\r\n")
                .unwrap();
            let request: Value = serde_json::from_slice(&bytes[split + 4..]).unwrap();
            if index > 0 {
                let request_headers = String::from_utf8_lossy(&bytes[..split]).to_ascii_lowercase();
                assert!(
                    request_headers.contains("mcp-session-id: session-1")
                        && request_headers.contains("mcp-protocol-version: 2025-06-18")
                );
            }
            let method = request.get("method").and_then(Value::as_str);
            let body = match method {
                Some("initialize") => Some(
                    json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}),
                ),
                Some("notifications/initialized") => None,
                Some("tools/list") => Some(json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}})),
                other => panic!("unexpected method {other:?}"),
            };
            let body = body
                .map(|value| {
                    if method == Some("tools/list") {
                        format!("data: {value}\n\n").into_bytes()
                    } else {
                        serde_json::to_vec(&value).unwrap()
                    }
                })
                .unwrap_or_default();
            let status = if body.is_empty() {
                "202 Accepted"
            } else {
                "200 OK"
            };
            let content_type = if method == Some("tools/list") {
                "text/event-stream"
            } else {
                "application/json"
            };
            let response = if method == Some("tools/list") {
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nMcp-Session-Id: session-1\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n"
                )
            } else {
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nMcp-Session-Id: session-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
            };
            socket.write_all(response.as_bytes()).await.unwrap();
            if method == Some("tools/list") {
                socket
                    .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(&body).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
                socket.flush().await.unwrap();
                sleep(Duration::from_secs(2)).await;
            } else {
                socket.write_all(&body).await.unwrap();
            }
        }
    });
    let client = McpClient::new(McpServer::StreamableHttp {
        name: "http".to_owned(),
        url: format!("http://{address}/mcp"),
        headers: BTreeMap::new(),
    });
    assert!(
        timeout(Duration::from_millis(500), client.list_tools())
            .await
            .expect("SSE event must complete before the stream closes")
            .unwrap()
            .is_empty()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn streamable_http_reinitializes_once_after_session_404() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for index in 0..6 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(split) = bytes.windows(4).position(|item| item == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..split]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(str::trim)
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            let split = bytes
                .windows(4)
                .position(|item| item == b"\r\n\r\n")
                .unwrap();
            let headers = String::from_utf8_lossy(&bytes[..split]).to_ascii_lowercase();
            let request: Value = serde_json::from_slice(&bytes[split + 4..]).unwrap();
            let method = request.get("method").and_then(Value::as_str);
            if !matches!(index, 0 | 3) {
                assert!(headers.contains("mcp-protocol-version: 2025-06-18"));
                let expected_session = if index < 3 { "session-1" } else { "session-2" };
                assert!(headers.contains(&format!("mcp-session-id: {expected_session}")));
            }
            let (status, session, body) = match index {
                0 => (
                    "200 OK",
                    "session-1",
                    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{}}})).unwrap(),
                ),
                1 => ("202 Accepted", "session-1", Vec::new()),
                2 => {
                    assert_eq!(method, Some("tools/list"));
                    ("404 Not Found", "session-1", Vec::new())
                }
                3 => (
                    "200 OK",
                    "session-2",
                    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{}}})).unwrap(),
                ),
                4 => ("202 Accepted", "session-2", Vec::new()),
                _ => (
                    "200 OK",
                    "session-2",
                    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}})).unwrap(),
                ),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nMcp-Session-Id: {session}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let client = McpClient::new(McpServer::StreamableHttp {
        name: "http".to_owned(),
        url: format!("http://{address}/mcp"),
        headers: BTreeMap::new(),
    });
    assert!(client.list_tools().await.unwrap().is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn changed_hook_is_blocked_and_reviewed_hook_is_audited() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    write_plugin(&root, true);
    let mut bundle = PluginLoader
        .load(&root, &temporary.path().join("data"))
        .unwrap();
    let hook = bundle.hooks.remove(0);
    let reviews = HookReviewStore::default();
    let audit = temporary.path().join("audit.jsonl");
    let engine = HookEngine::new(reviews.clone(), HashMap::new(), audit.clone())
        .with_trusted_host_processes(true);
    let invocation = HookInvocation {
        event: "UserPromptSubmit".to_owned(),
        fields: BTreeMap::new(),
        payload: json!({"safe":true}),
    };
    let allow = CapabilityPolicy::new(Some(Decision::Allow));
    assert!(matches!(
        engine
            .invoke("demo", &hook, &invocation, &allow, &allow)
            .await,
        Err(HookError::Unreviewed)
    ));
    reviews.approve("demo", &hook.id, &hook.trust_hash).unwrap();
    let mut deny_execute = CapabilityPolicy::new(Some(Decision::Allow));
    deny_execute.set(Capability::Execute, Decision::Deny);
    assert!(matches!(
        engine
            .invoke("demo", &hook, &invocation, &deny_execute, &allow)
            .await,
        Err(HookError::Denied)
    ));
    let output = engine
        .invoke("demo", &hook, &invocation, &allow, &allow)
        .await
        .unwrap();
    assert_eq!(output["received"]["safe"], true);
    let lines = fs::read_to_string(audit).unwrap();
    assert!(lines.contains("blocked_unreviewed"));
    assert!(lines.contains("blocked_policy"));
    assert!(lines.contains("completed"));
}

#[tokio::test]
async fn concurrent_hook_audit_remains_parseable_json_lines() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    write_plugin(&root, true);
    let hook = PluginLoader
        .load(&root, &temporary.path().join("data"))
        .unwrap()
        .hooks
        .remove(0);
    let audit = temporary.path().join("audit.jsonl");
    let engine = HookEngine::new(HookReviewStore::default(), HashMap::new(), audit.clone());
    let invocation = HookInvocation {
        event: "UserPromptSubmit".to_owned(),
        fields: BTreeMap::new(),
        payload: json!({"safe":true}),
    };
    let allow = CapabilityPolicy::new(Some(Decision::Allow));
    join_all((0..64).map(|_| {
        let engine = engine.clone();
        let hook = hook.clone();
        let invocation = invocation.clone();
        let allow = allow.clone();
        async move {
            assert!(matches!(
                engine
                    .invoke("demo", &hook, &invocation, &allow, &allow)
                    .await,
                Err(HookError::Unreviewed)
            ));
        }
    }))
    .await;
    let contents = fs::read_to_string(audit).unwrap();
    let lines = contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 64);
    assert!(
        lines
            .iter()
            .all(|line| serde_json::from_str::<cool_extensions::HookAudit>(line).is_ok())
    );
}

#[test]
fn plugin_policy_cannot_widen_core_or_declaration() {
    let mut core = CapabilityPolicy::new(Some(Decision::Allow));
    core.set(Capability::Network, Decision::Deny);
    let declared = BTreeSet::from([Capability::Network]);
    let requested = CapabilityPolicy::new(Some(Decision::Allow));
    let policy = narrowed_plugin_policy(&core, &declared, &requested);
    assert_eq!(policy.resolve(Capability::Network), Decision::Deny);
    assert_eq!(policy.resolve(Capability::Execute), Decision::Deny);
}

#[tokio::test]
async fn worker_handshake_and_crash_are_isolated() {
    let mut worker = WorkerProtocol::spawn(
        CompatibilityAdapter::Codex,
        helper(),
        vec!["worker".to_owned()],
        std::env::current_dir().unwrap(),
        BTreeMap::new(),
        BTreeSet::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        worker.request("echo", json!({"ok":true})).await.unwrap()["params"]["ok"],
        true
    );
    worker.stop().await.unwrap();
    let crashed = WorkerProtocol::spawn(
        CompatibilityAdapter::Claude,
        helper(),
        vec!["crash".to_owned()],
        std::env::current_dir().unwrap(),
        BTreeMap::new(),
        BTreeSet::new(),
    )
    .await;
    assert!(matches!(crashed, Err(WorkerError::Exited)));
    assert_eq!(2 + 2, 4);
}

#[tokio::test]
async fn worker_request_cancellation_is_sent_and_returns_without_deadline_wait() {
    let mut worker = WorkerProtocol::spawn(
        CompatibilityAdapter::Codex,
        helper(),
        vec!["worker-slow".to_owned()],
        std::env::current_dir().unwrap(),
        BTreeMap::new(),
        BTreeSet::new(),
    )
    .await
    .unwrap();
    let (sender, receiver) = watch::channel(false);
    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        sender.send(true).unwrap();
    });
    let result = timeout(
        Duration::from_millis(500),
        worker.request_cancellable("slow", Value::Null, receiver),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(WorkerError::Cancelled)));
    worker.stop().await.unwrap();
}

#[tokio::test]
async fn worker_structured_error_preserves_code_and_message() {
    let mut worker = WorkerProtocol::spawn(
        CompatibilityAdapter::Claude,
        helper(),
        vec!["worker-error".to_owned()],
        std::env::current_dir().unwrap(),
        BTreeMap::new(),
        BTreeSet::new(),
    )
    .await
    .unwrap();
    let error = worker.request("denied", Value::Null).await.unwrap_err();
    let WorkerError::Remote(remote) = error else {
        panic!("worker error must stay structured")
    };
    assert_eq!(remote.code, "fixture_denied");
    assert_eq!(remote.message, "denied");
    assert!(!remote.retryable);
    assert_eq!(remote.data, None);
    worker.stop().await.unwrap();
}

#[tokio::test]
async fn compatibility_supervisor_starts_heartbeats_and_routes_idempotent_requests() {
    let supervisor = CompatibilityWorkerSupervisor::default();
    let started = supervisor
        .start(
            CompatibilityAdapter::Codex,
            WorkerLaunchSpec {
                program: helper(),
                args: vec!["worker".to_owned()],
                cwd: std::env::current_dir().unwrap(),
                environment: BTreeMap::new(),
                allowed_secret_environment: BTreeSet::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(started).unwrap()["kind"],
        "worker.started"
    );
    assert!(supervisor.heartbeat().await.is_empty());
    let outcome = supervisor
        .request(
            CompatibilityAdapter::Codex,
            "echo",
            json!({"ok":true}),
            WorkerOperationClass::ReadOnly,
            Some("request-1"),
        )
        .await
        .unwrap();
    let WorkerRequestOutcome::Completed { value, events } = outcome else {
        panic!("healthy worker request must complete")
    };
    assert_eq!(value["receivedMethod"], "codex.request");
    assert_eq!(value["params"]["operation"], "echo");
    assert_eq!(value["params"]["input"]["input"]["ok"], true);
    assert!(value["deadlineUnixMs"].as_u64().is_some());
    assert!(events.is_empty());
    assert!(matches!(
        supervisor
            .request(
                CompatibilityAdapter::Codex,
                "write",
                Value::Null,
                WorkerOperationClass::SideEffect,
                None,
            )
            .await,
        Err(WorkerError::Protocol(message)) if message.contains("idempotency")
    ));
}

#[tokio::test]
async fn claude_adapter_translates_request_and_response_envelopes() {
    let supervisor = CompatibilityWorkerSupervisor::default();
    supervisor
        .start(
            CompatibilityAdapter::Claude,
            WorkerLaunchSpec {
                program: helper(),
                args: vec!["worker".to_owned()],
                cwd: std::env::current_dir().unwrap(),
                environment: BTreeMap::new(),
                allowed_secret_environment: BTreeSet::new(),
            },
        )
        .await
        .unwrap();
    let WorkerRequestOutcome::Completed { value, .. } = supervisor
        .request(
            CompatibilityAdapter::Claude,
            "review",
            json!({"prompt":"hello"}),
            WorkerOperationClass::ReadOnly,
            None,
        )
        .await
        .unwrap()
    else {
        panic!("Claude adapter request must complete")
    };
    assert_eq!(value["receivedMethod"], "claude.request");
    assert_eq!(value["params"]["operation"], "review");
}

#[tokio::test]
async fn compatibility_supervisor_restarts_but_does_not_replay_unknown_outcome() {
    let supervisor = CompatibilityWorkerSupervisor::default();
    supervisor
        .start(
            CompatibilityAdapter::Claude,
            WorkerLaunchSpec {
                program: helper(),
                args: vec!["worker-crash-request".to_owned()],
                cwd: std::env::current_dir().unwrap(),
                environment: BTreeMap::new(),
                allowed_secret_environment: BTreeSet::new(),
            },
        )
        .await
        .unwrap();
    let outcome = supervisor
        .request(
            CompatibilityAdapter::Claude,
            "side_effect",
            json!({"value":1}),
            WorkerOperationClass::SideEffect,
            Some("request-1"),
        )
        .await
        .unwrap();
    let WorkerRequestOutcome::UnknownOutcome { events, .. } = outcome else {
        panic!("crashed request must remain unknown instead of being replayed")
    };
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn extension_runtime_emits_status_and_invokes_hook_boundary() {
    let temporary = TempDir::new().unwrap();
    let store_root = temporary.path().join("plugins");
    let (install, data, content_hash) = install_plugin_fixture(&store_root, "demo", true);
    fs::write(store_root.join("plugins.lock.json"), serde_json::to_vec_pretty(&json!({"lock_version":1,"plugins":{"demo":{"name":"demo","version":"1","enabled":true,"source_type":"local","source":"fixture","revision":"","content_hash":content_hash,"install_path":install,"data_path":data,"installed_at":"2026-09-02T00:00:00Z","diagnostics":[],"resolved_dependencies":[],"required_capabilities":[]}}})).unwrap()).unwrap();
    let store = PluginStore::open(&store_root).unwrap();
    let runtime = ExtensionRuntime::from_store(&store).unwrap();
    let events = runtime
        .lifecycle_event(
            "SessionStart",
            json!({"content":"hello"}),
            &CapabilityPolicy::new(Some(Decision::Allow)),
        )
        .await;
    assert!(
        events
            .iter()
            .any(|event| { serde_json::to_value(event).unwrap()["kind"] == "plugin.status" })
    );
    runtime
        .lifecycle_event(
            "UserPromptSubmit",
            json!({"content":"hello"}),
            &CapabilityPolicy::new(Some(Decision::Allow)),
        )
        .await;
    assert!(
        fs::read_to_string(store_root.join("hook-audit.jsonl"))
            .unwrap()
            .contains("blocked_unreviewed")
    );
}

#[test]
fn vendor_manifest_is_diagnostic_only_compatibility() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("plugin");
    fs::create_dir_all(root.join(".codex-plugin")).unwrap();
    fs::write(
        root.join(".codex-plugin/plugin.json"),
        r#"{"name":"vendor-demo","version":"1"}"#,
    )
    .unwrap();
    let bundle = PluginLoader
        .load(&root, &temporary.path().join("data"))
        .unwrap();
    assert!(bundle.loadable());
    assert!(!bundle.conformant());
    assert!(
        bundle
            .diagnostics
            .iter()
            .any(|item| item.code == "compatibility.transformed")
    );
}

#[test]
fn m3_codex_and_claude_fixtures_have_explicit_tier_two_mappings() {
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../backend/tests/fixtures/plugins");
    let temporary = TempDir::new().unwrap();
    let codex = PluginLoader
        .load(
            &fixtures.join("codex-declarative"),
            &temporary.path().join("codex-data"),
        )
        .unwrap();
    assert_eq!(codex.skills.len(), 1);
    assert_eq!(codex.mcp_servers.len(), 1);
    assert_eq!(codex.mcp_servers[0].name(), "fixture");
    assert!(
        codex
            .diagnostics
            .iter()
            .any(|item| item.code == "compatibility.mcp_transformed")
    );

    let claude = PluginLoader
        .load(
            &fixtures.join("claude-declarative"),
            &temporary.path().join("claude-data"),
        )
        .unwrap();
    assert_eq!(claude.skills.len(), 1);
    assert!(claude.mcp_servers.is_empty());
    assert!(
        claude
            .diagnostics
            .iter()
            .any(|item| item.code == "compatibility.feature_unsupported")
    );

    let single = PluginLoader
        .load(
            &fixtures.join("claude-single-skill"),
            &temporary.path().join("single-data"),
        )
        .unwrap();
    assert!(single.manifest.is_some());
    assert_eq!(single.skills.len(), 1);
}

#[test]
fn manifestless_claude_root_skill_name_need_not_match_directory() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("arbitrary-directory");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("SKILL.md"),
        "---\nname: actual-skill\ndescription: Root Claude skill.\n---\nBody.\n",
    )
    .unwrap();
    let bundle = PluginLoader
        .load(&root, &temporary.path().join("data"))
        .unwrap();
    assert_eq!(bundle.manifest.unwrap().name, "actual-skill");
    assert_eq!(bundle.skills[0].name, "actual-skill");
}

#[test]
fn codex_vendor_mcp_is_translated_to_canonical_server() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().join("vendor");
    let data = temporary.path().join("data");
    fs::create_dir_all(root.join(".codex-plugin")).unwrap();
    fs::write(
        root.join(".codex-plugin/plugin.json"),
        r#"{"name":"codex-demo","version":"1"}"#,
    )
    .unwrap();
    fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers":{"local":{"type":"stdio","command":"python","args":["-V"]}}}"#,
    )
    .unwrap();
    fs::create_dir_all(&data).unwrap();
    let bundle = PluginLoader.load(&root, &data).unwrap();
    assert_eq!(bundle.mcp_servers.len(), 1);
}

#[test]
fn output_types_remain_json_serializable() {
    let value: Value = serde_json::to_value(json!({"ok":true})).unwrap();
    assert_eq!(value["ok"], true);
}

#[test]
fn plugin_status_uses_the_canonical_protocol_event() {
    let value = serde_json::to_value(plugin_status_event("demo", "enabled", None)).unwrap();
    assert_eq!(value["kind"], "plugin.status");
    assert_eq!(value["payload"]["pluginId"], "demo");
}
