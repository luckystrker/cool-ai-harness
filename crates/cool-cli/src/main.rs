use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, MessageRole,
    ModelDriver, OpenAiCompatibleDriver, RunOutcome, ScriptedDriver, StoreEventSink, ToolContext,
    builtin_registry,
};
use cool_app_server::{AppClient, AppServer, RunLifecycle, ServerConfig, capabilities};
use cool_extensions::{
    CompatibilityAdapter, ExtensionRuntime, InstalledPlugin, McpToolPolicy, PluginLoader,
    PluginStore, WorkerLaunchSpec, discover_plugin_tools_with_policy,
};
use cool_protocol::{ApprovalOutcome, CanonicalEvent, StatusEntry, StatusGetResult};
use cool_security::{
    CapabilityPolicy, Decision, NetworkPolicy, SecretKey, SecretKeyring, Workspace,
};
use cool_state::DurableStore;
use serde_json::json;

#[tokio::main]
async fn main() {
    if let Err((code, message)) = run().await {
        eprintln!(
            "{}",
            serde_json::to_string(&message).expect("error JSON serializes")
        );
        std::process::exit(code);
    }
}

async fn run() -> Result<(), (i32, serde_json::Value)> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        return run_tui(args.collect()).await;
    };
    match command.as_str() {
        "app-server" => {
            let mut transport = "stdio".to_owned();
            let mut endpoint: Option<PathBuf> = None;
            let mut data_dir = default_data_dir();
            let mut legacy_store = false;
            while let Some(argument) = args.next() {
                match argument.as_str() {
                    "--transport" => {
                        transport = args
                            .next()
                            .ok_or_else(|| usage("missing transport value"))?;
                    }
                    "--endpoint" => {
                        endpoint = Some(PathBuf::from(
                            args.next().ok_or_else(|| usage("missing endpoint value"))?,
                        ));
                    }
                    "--data-dir" => {
                        data_dir = PathBuf::from(
                            args.next().ok_or_else(|| usage("missing data directory"))?,
                        );
                    }
                    "--legacy-store" => legacy_store = true,
                    _ => return Err(usage("unknown app-server argument")),
                }
            }
            match transport.as_str() {
                "stdio" if endpoint.is_none() => {}
                "local" if endpoint.is_some() => {}
                "local" => return Err(usage("local transport needs endpoint")),
                _ => return Err(usage("transport must be stdio or local")),
            }
            let server = build_server(&data_dir, legacy_store).await?;
            match transport.as_str() {
                "stdio" => server
                    .serve_stdio()
                    .await
                    .map_err(|error| runtime("app_server_failed", &error.to_string())),
                "local" => {
                    let endpoint = endpoint.expect("validated local endpoint");
                    server
                        .serve_local(&endpoint)
                        .await
                        .map_err(|error| runtime("local_transport_failed", &error.to_string()))
                }
                _ => unreachable!("validated transport"),
            }
        }
        "doctor" => {
            let mut data_dir = default_data_dir();
            let mut remaining = args.peekable();
            while let Some(argument) = remaining.next() {
                match argument.as_str() {
                    "--data-dir" => {
                        data_dir = PathBuf::from(
                            remaining
                                .next()
                                .ok_or_else(|| usage("missing data directory"))?,
                        );
                    }
                    _ => return Err(usage("unknown doctor argument")),
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "status": "ok",
                    "phase": "M10",
                    "runtime": "rust-trusted-core",
                    "protocolVersion": 1,
                    "capabilities": capabilities(),
                    "durableState": true,
                    "securityKernel": true,
                    "agentLoop": true,
                    "trustedTools": true,
                    "baselineProvider": "openai-compatible",
                    "plugins": true,
                    "pluginInstall": ["local", "git-pinned"],
                    "mcp": ["stdio", "streamable-http"],
                    "hooks": true,
                    "compatibilityWorkers": ["codex", "claude"],
                    "tui": true,
                    "acp": true,
                    "legacyStore": inspect_legacy_store(&data_dir)
                }))
                .expect("doctor JSON serializes")
            );
            Ok(())
        }
        "plugin" => plugin_command(args.collect()),
        "mcp" => mcp_command(args.collect()),
        "hooks" => hooks_command(args.collect()),
        "run" => run_prompt(args.collect()).await,
        "acp" => run_acp(default_data_dir()).await,
        "tui" | "chat" => run_tui(args.collect()).await,
        "serve" => Err((
            2,
            json!({
                "coolCode": "m11_route_not_implemented",
                "message": format!("{command} is routed but becomes operational in a later phase"),
                "retryable": false
            }),
        )),
        "--version" | "-V" => {
            println!("cool {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        _ => Err(usage("unknown command")),
    }
}

fn default_data_dir() -> PathBuf {
    env::var_os("COOL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"))
}

/// Read-only inspection of the legacy database for `cool doctor`.
///
/// Never adopts or writes: the doctor reports whether `harness.db` is still
/// Python-owned or already adopted by the Rust store, together with the
/// Alembic baseline and Rust schema version.
fn inspect_legacy_store(data_dir: &std::path::Path) -> serde_json::Value {
    let database = data_dir.join("harness.db");
    if !database.exists() {
        return json!({"path": database.to_string_lossy(), "status": "absent"});
    }
    match cool_store::LegacyStore::open_read_only(&database) {
        Ok(store) => {
            let meta = store.meta().ok();
            let owner = meta.as_ref().and_then(|meta| meta.owner.clone());
            json!({
                "path": database.to_string_lossy(),
                "status": if owner.as_deref() == Some("rust") {
                    "rust-owned"
                } else {
                    "python-owned"
                },
                "alembicRevision": store.alembic_revision().ok().flatten(),
                "schemaVersion": store.schema_version().ok(),
                "adoptedAt": meta.and_then(|meta| meta.adopted_at),
            })
        }
        Err(error) => json!({
            "path": database.to_string_lossy(),
            "status": "error",
            "error": error.to_string(),
        }),
    }
}

/// Open `harness.db` for the app server.
///
/// A Rust-owned store opens normally (ownership is verified and pending Rust
/// migrations run). A Python-owned store is opened **read-only**: adoption is a
/// migration decision, not a side effect of starting a server. This is only
/// reachable through the explicit `--legacy-store` flag.
fn open_legacy_store(
    database: &std::path::Path,
) -> Result<cool_store::LegacyStore, (i32, serde_json::Value)> {
    let read_only = match cool_store::LegacyStore::open_read_only(database) {
        Ok(store) => !store
            .meta()
            .map(|meta| meta.owner.as_deref() == Some("rust"))
            .unwrap_or(false),
        Err(error) => return Err(runtime("legacy_store_failed", &error.to_string())),
    };
    let options = cool_store::StoreOptions {
        read_only,
        ..cool_store::StoreOptions::default()
    };
    cool_store::LegacyStore::open(database, &options)
        .map_err(|error| runtime("legacy_store_failed", &error.to_string()))
}

fn configured_secrets() -> Option<Arc<SecretKeyring>> {
    let secret = env::var("SECRET_KEY")
        .ok()
        .filter(|value| !value.is_empty())?;
    let key = SecretKey::from_secret("default", &secret, false).ok()?;
    Some(Arc::new(SecretKeyring::new(key, std::iter::empty())))
}

async fn build_server(
    data_dir: &std::path::Path,
    legacy_store: bool,
) -> Result<AppServer, (i32, serde_json::Value)> {
    let store = DurableStore::open(data_dir.join("rust-core.db"))
        .map_err(|error| runtime("durable_state_failed", &error.to_string()))?;
    let legacy = if legacy_store {
        let database = data_dir.join("harness.db");
        if !database.exists() {
            return Err(runtime(
                "legacy_store_missing",
                "harness.db was not found under the data directory",
            ));
        }
        Some(Arc::new(open_legacy_store(&database)?))
    } else {
        None
    };
    let config = ServerConfig {
        secrets: configured_secrets(),
        legacy_store: legacy,
        ..ServerConfig::default()
    };
    let (provider, model) = configured_provider(config.event_delay, true)?;
    let workspace = current_workspace()?;
    let (registry, extensions) = extension_registry(data_dir).await;
    let agent = AgentRuntime::new(provider, registry);
    let mut server = AppServer::with_agent_runtime(
        config,
        store,
        agent,
        workspace,
        CapabilityPolicy::new(Some(Decision::Ask)),
        model,
    )
    .map_err(|error| runtime("durable_recovery_failed", &error.to_string()))?;
    if let Some(extensions) = extensions {
        server = server.with_run_lifecycle(Arc::new(CliExtensions(extensions)));
    }
    Ok(server)
}

async fn extension_registry(
    data_dir: &std::path::Path,
) -> (cool_agent::ToolRegistry, Option<ExtensionRuntime>) {
    let mut registry = builtin_registry();
    let Ok(store) = PluginStore::open(data_dir.join("plugins")) else {
        return (registry, None);
    };
    let runtime = ExtensionRuntime::from_store(&store).ok();
    if let Some(runtime) = &runtime {
        start_configured_worker(runtime, CompatibilityAdapter::Codex, "COOL_CODEX_WORKER").await;
        start_configured_worker(runtime, CompatibilityAdapter::Claude, "COOL_CLAUDE_WORKER").await;
    }
    let policy_path = data_dir.join("plugins").join("mcp-tool-policy.json");
    let tool_policy = if policy_path.exists() {
        match std::fs::read(&policy_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<McpToolPolicy>(&bytes).ok())
        {
            Some(policy) => policy,
            None => {
                if let Some(runtime) = &runtime {
                    runtime
                        .report_plugin_status(
                            "core/mcp-policy",
                            "failed",
                            Some("invalid mcp-tool-policy.json".to_owned()),
                        )
                        .await;
                }
                McpToolPolicy::deny_all()
            }
        }
    } else {
        McpToolPolicy::default()
    };
    let Ok(entries) = store.load_enabled_isolated() else {
        return (registry, runtime);
    };
    for bundle in entries.into_iter().flatten() {
        let Some(manifest) = bundle.manifest else {
            continue;
        };
        for server in bundle.mcp_servers {
            match discover_plugin_tools_with_policy(&manifest.name, server, &tool_policy).await {
                Ok(tools) => match registry.extend(tools) {
                    Ok(extended) => registry = extended,
                    Err(error) => {
                        if let Some(runtime) = &runtime {
                            runtime
                                .report_plugin_status(
                                    &manifest.name,
                                    "degraded",
                                    Some(format!("tool_registry: {error}")),
                                )
                                .await;
                        }
                    }
                },
                Err(error) => {
                    if let Some(runtime) = &runtime {
                        runtime
                            .report_plugin_status(
                                &manifest.name,
                                "degraded",
                                Some(format!("mcp_discovery: {error}")),
                            )
                            .await;
                    }
                }
            }
        }
    }
    (registry, runtime)
}

fn current_workspace() -> Result<Workspace, (i32, serde_json::Value)> {
    Workspace::new(
        env::current_dir().map_err(|error| runtime("workspace_failed", &error.to_string()))?,
    )
    .map_err(|error| runtime("workspace_failed", &error.to_string()))
}

struct CliExtensions(ExtensionRuntime);

#[async_trait]
impl RunLifecycle for CliExtensions {
    async fn on_event(
        &self,
        event: &str,
        payload: serde_json::Value,
        policy: &CapabilityPolicy,
    ) -> Vec<CanonicalEvent> {
        self.0.lifecycle_event(event, payload, policy).await
    }

    async fn status(&self) -> Option<StatusGetResult> {
        let mut plugins: BTreeMap<String, StatusEntry> = BTreeMap::new();
        let mut workers: BTreeMap<String, StatusEntry> = BTreeMap::new();
        for event in self.0.status_events().await {
            match event {
                CanonicalEvent::PluginStatus(status) => {
                    plugins.insert(
                        status.plugin_id.clone(),
                        StatusEntry {
                            id: status.plugin_id,
                            status: status.status,
                            code: status.code,
                        },
                    );
                }
                CanonicalEvent::WorkerStarted(worker) | CanonicalEvent::WorkerRestarted(worker) => {
                    workers.insert(
                        worker.worker_id.clone(),
                        StatusEntry {
                            id: worker.worker_id,
                            status: "running".to_owned(),
                            code: worker.code,
                        },
                    );
                }
                CanonicalEvent::WorkerFailed(worker) => {
                    workers.insert(
                        worker.worker_id.clone(),
                        StatusEntry {
                            id: worker.worker_id,
                            status: "failed".to_owned(),
                            code: worker.code,
                        },
                    );
                }
                _ => {}
            }
        }
        Some(StatusGetResult {
            plugins: plugins.into_values().collect(),
            workers: workers.into_values().collect(),
            mcp_servers: self.0.mcp_server_names(),
        })
    }
}

async fn run_acp(data_dir: PathBuf) -> Result<(), (i32, serde_json::Value)> {
    let workspace =
        env::current_dir().map_err(|error| runtime("workspace_failed", &error.to_string()))?;
    let (client, child) = spawn_app_server(&data_dir).await?;
    let result = async {
        client
            .initialize("cool-acp", env!("CARGO_PKG_VERSION"))
            .await
            .map_err(|error| runtime("acp_initialize_failed", &error.to_string()))?;
        cool_acp::run_stdio(client, workspace)
            .await
            .map_err(|error| runtime("acp_failed", &error.to_string()))
    }
    .await;
    if let Some(mut child) = child {
        let _ = child.kill().await;
    }
    result
}

async fn run_tui(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    if !arguments.is_empty() {
        return Err(usage("cool takes no arguments"));
    }
    if !std::io::stdin().is_terminal() {
        return Err(runtime(
            "tui_requires_terminal",
            "the interactive TUI needs a terminal on stdin; use `cool run` or `cool acp` otherwise",
        ));
    }
    let data_dir = default_data_dir();
    let (client, child) = spawn_app_server(&data_dir).await?;
    let result = cool_tui::run_terminal(client)
        .await
        .map_err(|error| runtime("tui_failed", &error.to_string()));
    if let Some(mut child) = child {
        let _ = child.kill().await;
    }
    result
}

/// Spawns `cool app-server --transport stdio` as a supervised child process.
async fn spawn_app_server(
    data_dir: &std::path::Path,
) -> Result<(AppClient, Option<tokio::process::Child>), (i32, serde_json::Value)> {
    let binary =
        env::current_exe().map_err(|error| runtime("tui_spawn_failed", &error.to_string()))?;
    let mut child = tokio::process::Command::new(binary)
        .arg("app-server")
        .arg("--transport")
        .arg("stdio")
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| runtime("tui_spawn_failed", &error.to_string()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| runtime("tui_spawn_failed", "child stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| runtime("tui_spawn_failed", "child stdout is unavailable"))?;
    let client = AppClient::connect(stdout, stdin)
        .map_err(|error| runtime("tui_spawn_failed", &error.to_string()))?;
    Ok((client, Some(child)))
}

fn plugin_command(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    let Some(action) = arguments.first().map(String::as_str) else {
        return Err(usage(
            "plugin command is: plugin install|list|validate|doctor [argument]",
        ));
    };
    let data_dir = default_data_dir();
    match action {
        "install" => {
            let Some(source) = arguments.get(1) else {
                return Err(usage("plugin install needs a source"));
            };
            let revision = arguments
                .iter()
                .position(|value| value == "--revision")
                .and_then(|index| arguments.get(index + 1))
                .cloned();
            let store = open_plugin_store(&data_dir)?;
            let entry = match revision {
                Some(revision) => store.install_git(source, &revision),
                None => store.install_local(std::path::Path::new(source)),
            }
            .map_err(|error| runtime("plugin_install_failed", &error.to_string()))?;
            print_json(&plugin_entry_json(&entry))
        }
        "list" => {
            let store = open_plugin_store(&data_dir)?;
            let entries = store
                .list()
                .map_err(|error| runtime("plugin_store_failed", &error.to_string()))?;
            print_json(&json!({
                "plugins": entries.iter().map(plugin_entry_json).collect::<Vec<_>>(),
            }))
        }
        "validate" => {
            let Some(path) = arguments.get(1) else {
                return Err(usage("plugin validate needs a path"));
            };
            let root = PathBuf::from(path);
            let data = root
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(".plugin-data");
            let bundle = PluginLoader
                .load(&root, &data)
                .map_err(|error| runtime("plugin_load_failed", &error.to_string()))?;
            print_json(&json!({
                "name": bundle.manifest.as_ref().map(|value| &value.name),
                "loadable": bundle.loadable(),
                "conformant": bundle.conformant(),
                "contentHash": bundle.content_hash,
                "skills": bundle.skills,
                "mcpServers": bundle.mcp_servers.len(),
                "hooks": bundle.hooks.len(),
                "diagnostics": bundle.diagnostics,
            }))
        }
        "doctor" => {
            let target = arguments.get(1).cloned();
            if let Some(path) = target
                .as_ref()
                .filter(|value| std::path::Path::new(value.as_str()).is_dir())
            {
                return plugin_command(vec!["validate".to_owned(), path.clone()]);
            }
            let store = open_plugin_store(&data_dir)?;
            let entries = store
                .list()
                .map_err(|error| runtime("plugin_store_failed", &error.to_string()))?;
            let mut reports = Vec::new();
            for entry in entries {
                if let Some(name) = &target
                    && name != &entry.name
                {
                    continue;
                }
                let bundle = PluginLoader
                    .load(
                        std::path::Path::new(&entry.install_path),
                        std::path::Path::new(&entry.data_path),
                    )
                    .ok();
                reports.push(json!({
                    "name": entry.name,
                    "enabled": entry.enabled,
                    "sourceType": entry.source_type,
                    "source": entry.source,
                    "revision": entry.revision,
                    "contentHash": entry.content_hash,
                    "installPath": entry.install_path,
                    "dataPath": entry.data_path,
                    "requiredCapabilities": entry.required_capabilities,
                    "resolvedDependencies": entry.resolved_dependencies,
                    "loadable": bundle.as_ref().is_some_and(|bundle| bundle.loadable()),
                    "conformant": bundle.as_ref().is_some_and(|bundle| bundle.conformant()),
                    "diagnostics": bundle.as_ref().map(|bundle| bundle.diagnostics.clone()),
                }));
            }
            print_json(&json!({"plugins": reports}))
        }
        _ => Err(usage("unknown plugin action")),
    }
}

fn mcp_command(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    if arguments.first().map(String::as_str) != Some("list") {
        return Err(usage("mcp command is: mcp list"));
    }
    let store = open_plugin_store(&default_data_dir())?;
    let entries = store
        .list()
        .map_err(|error| runtime("plugin_store_failed", &error.to_string()))?;
    let mut servers = Vec::new();
    for entry in &entries {
        if !entry.enabled {
            continue;
        }
        let Ok(bundles) = store.load_enabled_isolated() else {
            continue;
        };
        for bundle in bundles.into_iter().flatten() {
            let Some(manifest) = &bundle.manifest else {
                continue;
            };
            if manifest.name != entry.name {
                continue;
            }
            for server in bundle.mcp_servers {
                let (transport, endpoint) = match &server {
                    cool_extensions::McpServer::Stdio { command, .. } => {
                        ("stdio", command.to_string_lossy().into_owned())
                    }
                    cool_extensions::McpServer::StreamableHttp { url, .. } => {
                        ("streamable_http", url.clone())
                    }
                };
                servers.push(json!({
                    "plugin": manifest.name,
                    "name": server.name(),
                    "transport": transport,
                    "endpoint": endpoint,
                }));
            }
        }
    }
    print_json(&json!({"servers": servers}))
}

fn hooks_command(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    if arguments.first().map(String::as_str) != Some("list") {
        return Err(usage("hooks command is: hooks list"));
    }
    let store = open_plugin_store(&default_data_dir())?;
    let entries = store
        .list()
        .map_err(|error| runtime("plugin_store_failed", &error.to_string()))?;
    let mut hooks = Vec::new();
    for entry in &entries {
        if !entry.enabled {
            continue;
        }
        let Ok(bundles) = store.load_enabled_isolated() else {
            continue;
        };
        let reviewed = store.reviewed_hook_hashes(&entry.name).unwrap_or_default();
        for bundle in bundles.into_iter().flatten() {
            let Some(manifest) = &bundle.manifest else {
                continue;
            };
            if manifest.name != entry.name {
                continue;
            }
            for hook in bundle.hooks {
                let trusted = reviewed
                    .get(&hook.id)
                    .is_some_and(|hash| hash == &hook.trust_hash);
                let handler = match &hook.handler {
                    cool_extensions::HookHandler::Command { command, .. } => {
                        format!("command:{}", command.to_string_lossy())
                    }
                    cool_extensions::HookHandler::Mcp { server, tool, .. } => {
                        format!("mcp:{server}/{tool}")
                    }
                };
                hooks.push(json!({
                    "plugin": manifest.name,
                    "id": hook.id,
                    "event": hook.event,
                    "handler": handler,
                    "order": hook.order,
                    "parallel": hook.parallel,
                    "capabilities": hook.capabilities.iter().map(|value| value.as_str()).collect::<Vec<_>>(),
                    "trusted": trusted,
                }));
            }
        }
    }
    print_json(&json!({"hooks": hooks}))
}

fn open_plugin_store(data_dir: &std::path::Path) -> Result<PluginStore, (i32, serde_json::Value)> {
    PluginStore::open(data_dir.join("plugins"))
        .map_err(|error| runtime("plugin_store_failed", &error.to_string()))
}

fn plugin_entry_json(entry: &InstalledPlugin) -> serde_json::Value {
    json!({
        "name": entry.name,
        "version": entry.version,
        "enabled": entry.enabled,
        "sourceType": entry.source_type,
        "source": entry.source,
        "revision": entry.revision,
        "contentHash": entry.content_hash,
        "installPath": entry.install_path,
        "dataPath": entry.data_path,
        "installedAt": entry.installed_at,
        "requiredCapabilities": entry.required_capabilities,
        "resolvedDependencies": entry.resolved_dependencies,
    })
}

fn print_json(value: &serde_json::Value) -> Result<(), (i32, serde_json::Value)> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("CLI JSON serializes")
    );
    Ok(())
}

async fn start_configured_worker(
    runtime: &ExtensionRuntime,
    adapter: CompatibilityAdapter,
    variable: &str,
) {
    let Some(program) = env::var_os(variable).map(PathBuf::from) else {
        return;
    };
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _ = runtime
        .start_worker(
            adapter,
            WorkerLaunchSpec {
                program,
                args: Vec::new(),
                cwd,
                environment: BTreeMap::new(),
                allowed_secret_environment: BTreeSet::new(),
            },
        )
        .await;
}

async fn run_prompt(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    let (scripted, prompt_parts) = match arguments.first().map(String::as_str) {
        Some("--scripted") => (true, &arguments[1..]),
        _ => (false, arguments.as_slice()),
    };
    if prompt_parts.is_empty() {
        return Err(usage("run needs a prompt"));
    }
    let prompt = prompt_parts.join(" ");
    let workspace = current_workspace()?;
    let (provider, model): (Arc<dyn ModelDriver>, String) = if scripted {
        (Arc::new(ScriptedDriver::echo()), "scripted-echo".to_owned())
    } else {
        configured_provider(std::time::Duration::ZERO, false)?
    };
    let store = DurableStore::in_memory()
        .map_err(|error| runtime("durable_state_failed", &error.to_string()))?;
    let session = store
        .create_session(
            "local-user",
            "cli-session",
            "cli-session",
            Some("CLI"),
            None,
        )
        .map_err(|error| runtime("durable_state_failed", &error.to_string()))?
        .value;
    let run = store
        .start_run("local-user", "cli-run", "cli-run", &session)
        .map_err(|error| runtime("durable_state_failed", &error.to_string()))?
        .value;
    let sink = StoreEventSink::new(store, "local-user", session, run);
    let agent = AgentRuntime::new(provider, builtin_registry());
    let (_, cancel) = CancelSignal::channel();
    let outcome = agent
        .run(
            AgentRequest {
                model,
                history: Vec::new(),
                user_input: prompt,
                system_prompt: None,
                temperature: 0.7,
                max_tokens: None,
                limits: AgentLimits::default(),
                tool_names: None,
                tool_context: ToolContext::new(
                    workspace,
                    CapabilityPolicy::new(Some(Decision::Ask)),
                ),
            },
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Denied,
            },
            cancel,
        )
        .await
        .map_err(|error| runtime("agent_runtime_failed", &error.to_string()))?;
    match outcome {
        RunOutcome::Completed { history, .. } => {
            let output = history
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
                .and_then(|message| message.content.as_deref())
                .unwrap_or_default();
            println!("{output}");
            Ok(())
        }
        RunOutcome::Cancelled { reason, .. } => Err(runtime("run_cancelled", &reason)),
        RunOutcome::Failed { code, .. } => Err(runtime(&code, "agent run failed")),
    }
}

fn configured_provider(
    echo_delay: std::time::Duration,
    allow_scripted_fallback: bool,
) -> Result<(Arc<dyn ModelDriver>, String), (i32, serde_json::Value)> {
    let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
    let configured_base_url = env::var("OPENAI_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty());
    if api_key.is_empty() && configured_base_url.is_none() {
        if allow_scripted_fallback {
            return Ok((
                Arc::new(ScriptedDriver::echo_with_delay(echo_delay)),
                "scripted-echo".to_owned(),
            ));
        }
        return Err(runtime(
            "provider_credentials_missing",
            "OPENAI_API_KEY or an explicit OPENAI_BASE_URL is required; use --scripted only for deterministic local checks",
        ));
    }
    let base_url = configured_base_url.unwrap_or_else(|| "https://api.openai.com/v1/".to_owned());
    let parsed = url::Url::parse(&base_url)
        .map_err(|error| runtime("provider_config_invalid", &error.to_string()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| runtime("provider_config_invalid", "provider URL has no host"))?;
    let allow_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    let policy = if allow_loopback {
        NetworkPolicy::new([host.to_owned()]).loopback_only()
    } else {
        NetworkPolicy::new([host.to_owned()])
    };
    let provider = OpenAiCompatibleDriver::new(&base_url, api_key, policy)
        .map_err(|error| runtime("provider_config_invalid", &error.to_string()))?;
    let model = env::var("OPENAI_MODEL")
        .or_else(|_| env::var("OPENAI_DEFAULT_MODEL"))
        .unwrap_or_else(|_| "gpt-5-mini".to_owned());
    Ok((Arc::new(provider), model))
}

fn usage(message: &str) -> (i32, serde_json::Value) {
    (
        2,
        json!({"coolCode": "invalid_cli_usage", "message": message, "retryable": false}),
    )
}

fn runtime(code: &str, message: &str) -> (i32, serde_json::Value) {
    (
        1,
        json!({"coolCode": code, "message": message, "retryable": false}),
    )
}

fn print_help() {
    println!(
        "Cool Rust CLI\n\nCommands:\n  (no arguments)              interactive TUI\n  app-server [--transport stdio|local] [--endpoint PATH] [--data-dir PATH] [--legacy-store]\n  serve\n  run [--scripted] <prompt>\n  acp                         ACP v1 agent over stdio\n  plugin install <path|git-url> [--revision SHA]\n  plugin list\n  plugin validate <path>\n  plugin doctor [path]\n  mcp list\n  hooks list\n  doctor [--data-dir PATH]"
    );
}
