mod executor_tools;
mod mcp_admin;
mod mcp_store;
mod memory_extract;
mod provider_probe;
mod rss_feed;
mod skills_admin;
mod store_tools;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AnthropicDriver, AutoApprovalGate, CancelSignal,
    MessageRole, ModelDriver, OpenAiCompatibleDriver, RunOutcome, ScriptedDriver, StoreEventSink,
    ToolContext, builtin_registry,
};
use cool_app_server::{
    AppClient, AppServer, AppSettings, ExtensionAdmin, McpAdmin, RunLifecycle, ServerConfig,
    capabilities,
};
use cool_extensions::{
    CompatibilityAdapter, ExtensionRuntime, HookDeclaration, InstalledPlugin, McpToolPolicy,
    OpenCodeWorkerConfig, PluginBundle, PluginLoader, PluginStore, WorkerLaunchSpec,
    discover_plugin_tools_with_policy, opencode_launch_spec,
};
use cool_protocol::{
    ApprovalOutcome, CanonicalEvent, ExtensionDiagnosticRecord, ExtensionStatusResult, HookRecord,
    McpServerRecord, McpToolPolicyRecord, PluginRecord, SkillRecord, StatusEntry, StatusGetResult,
    SystemPromptRecord, WorkerRecord,
};
use cool_security::{
    CapabilityPolicy, Decision, NetworkPolicy, SecretKey, SecretKeyring, Workspace, mask_secrets,
};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use serde_json::json;

fn main() {
    // The App Protocol dispatch future is very large (one arm per command).
    // Give worker threads 8 MiB stacks (the 2 MiB default is tight on Windows),
    // and drive `run` on a worker rather than the 1 MiB main stack.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async {
        tokio::spawn(async {
            if let Err((code, message)) = run().await {
                eprintln!(
                    "{}",
                    serde_json::to_string(&message).expect("error JSON serializes")
                );
                std::process::exit(code);
            }
        })
        .await
        .expect("run task");
    });
}

async fn run() -> Result<(), (i32, serde_json::Value)> {
    // Collect first so no `std::env::Args` is held across an await (it is not `Send`).
    let mut args = env::args().skip(1).collect::<Vec<_>>().into_iter();
    let Some(command) = args.next() else {
        return run_tui(args.collect()).await;
    };
    match command.as_str() {
        "app-server" => {
            let mut transport = "stdio".to_owned();
            let mut endpoint: Option<PathBuf> = None;
            let mut data_dir = default_data_dir();
            let mut legacy_store = false;
            let mut process_launcher: Option<String> = None;
            let mut sandbox: Option<String> = None;
            let mut allow_shell = false;
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
                    "--process-launcher" => {
                        process_launcher = Some(
                            args.next()
                                .ok_or_else(|| usage("missing process-launcher value"))?,
                        );
                    }
                    "--sandbox" => {
                        sandbox = Some(args.next().ok_or_else(|| usage("missing sandbox value"))?);
                    }
                    "--allow-shell" => allow_shell = true,
                    _ => return Err(usage("unknown app-server argument")),
                }
            }
            match transport.as_str() {
                "stdio" if endpoint.is_none() => {}
                "local" if endpoint.is_some() => {}
                "local" => return Err(usage("local transport needs endpoint")),
                _ => return Err(usage("transport must be stdio or local")),
            }
            let host =
                cli_host(process_launcher, sandbox, allow_shell).map_err(|error| usage(&error))?;
            let server = build_server(&data_dir, legacy_store, host).await?;
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
                    "phase": "M11",
                    "runtime": "rust-trusted-core",
                    "protocolVersion": 1,
                    "capabilities": capabilities(),
                    "durableState": true,
                    "securityKernel": true,
                    "agentLoop": true,
                    "trustedTools": true,
                    "baselineProvider": "openai-compatible",
                    "processLauncher": {
                        "default": "disabled",
                        "envOverride": env::var("COOL_PROCESS_LAUNCHER").ok(),
                        "sandboxBackends": cool_agent::sandbox_backend_status()
                    },
                    "plugins": true,
                    "pluginInstall": ["local", "git-pinned"],
                    "mcp": ["stdio", "streamable-http"],
                    "hooks": true,
                    "compatibilityWorkers": ["codex", "claude"],
                    "tui": true,
                    "acp": true,
                    "webFacade": true,
                    "serveProfiles": ["local", "server"],
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
        "serve" => serve_command(args.collect()).await,
        "store" => store_command(args.collect()).await,
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
/// migration decision (`cool store adopt`), not a side effect of starting a
/// server. When the data root is fresh (no `harness.db` yet) the baseline
/// schema is initialized and Rust takes ownership in one transaction, so a
/// base install with no Python still serves the legacy families.
fn open_legacy_store(
    database: &std::path::Path,
) -> Result<cool_store::LegacyStore, (i32, serde_json::Value)> {
    let options = if database.exists() {
        let read_only = match cool_store::LegacyStore::open_read_only(database) {
            Ok(store) => !store
                .meta()
                .map(|meta| meta.owner.as_deref() == Some("rust"))
                .unwrap_or(false),
            Err(error) => return Err(runtime("legacy_store_failed", &error.to_string())),
        };
        cool_store::StoreOptions {
            read_only,
            ..cool_store::StoreOptions::default()
        }
    } else {
        cool_store::StoreOptions {
            initialize_if_missing: true,
            ..cool_store::StoreOptions::default()
        }
    };
    let writable = !options.read_only;
    let store = cool_store::LegacyStore::open(database, &options)
        .map_err(|error| runtime("legacy_store_failed", &error.to_string()))?;
    // Seed the builtin subagent roles that `spawn_subagent` resolves by name.
    // A Python-owned (read-only) store is left untouched.
    if writable && let Err(error) = store.seed_builtin_roles() {
        eprintln!("subagent role seeding skipped: {error}");
    }
    Ok(store)
}

fn configured_secrets() -> Option<Arc<SecretKeyring>> {
    let secret = env::var("SECRET_KEY")
        .ok()
        .filter(|value| !value.is_empty())?;
    let key = SecretKey::from_secret("default", &secret, false).ok()?;
    Some(Arc::new(SecretKeyring::new(key, std::iter::empty())))
}

/// Launcher selection for the CLI surface (P0.3/P2.17).
///
/// Chain: `COOL_PROCESS_LAUNCHER` env → explicit flag → default disabled.
/// `--allow-shell` is the `--process-launcher=host` shorthand; `--sandbox`
/// selects the sandbox backend (`bwrap|seatbelt|jobobject`) and implies
/// `--process-launcher=sandboxed`. Unknown or unavailable values fail closed.
fn cli_launcher(
    process_launcher: Option<String>,
    sandbox: Option<String>,
    allow_shell: bool,
) -> Result<Arc<dyn cool_agent::ProcessLauncher>, String> {
    if allow_shell {
        if process_launcher.is_some() {
            return Err("--allow-shell conflicts with --process-launcher".to_owned());
        }
        return Ok(Arc::new(cool_agent::HostLauncher));
    }
    let backend = sandbox
        .map(|value| {
            cool_agent::SandboxBackend::parse(&value)
                .ok_or_else(|| format!("unknown sandbox backend '{value}'"))
        })
        .transpose()?;
    match process_launcher {
        None => match backend {
            Some(backend) => cool_agent::resolve_launcher("sandboxed", Some(backend)),
            None => Ok(Arc::new(cool_agent::DisabledLauncher)),
        },
        Some(kind) => cool_agent::resolve_launcher(&kind, backend),
    }
}

/// The `HostContext` for a CLI-built server/context: the launcher from the
/// selection chain plus the host environment that launched processes inherit
/// (only populated when a launcher is enabled — a disabled launcher runs no
/// processes at all).
fn cli_host(
    process_launcher: Option<String>,
    sandbox: Option<String>,
    allow_shell: bool,
) -> Result<cool_agent::HostContext, String> {
    // `COOL_PROCESS_LAUNCHER`/`COOL_SANDBOX_BACKEND` override the flags.
    let launcher = match cool_agent::launcher_from_env() {
        Ok(Some(launcher)) => launcher,
        Ok(None) => cli_launcher(process_launcher, sandbox, allow_shell)?,
        Err(error) => return Err(error),
    };
    let environment = if launcher.kind() == cool_agent::LauncherKind::Disabled {
        std::collections::HashMap::new()
    } else {
        std::env::vars().collect()
    };
    Ok(cool_agent::HostContext {
        launcher,
        environment,
        rules: std::sync::Arc::new(cool_security::RuleState::default()),
    })
}

async fn build_server(
    data_dir: &std::path::Path,
    legacy_store: bool,
    host: cool_agent::HostContext,
) -> Result<AppServer, (i32, serde_json::Value)> {
    // Resolve the legacy store before creating rust-core.db so a misconfigured
    // `--legacy-store` exits without touching the data directory. A fresh data
    // root is initialized at the baseline; an existing Python-owned store stays
    // read-only until `cool store adopt`.
    let legacy = if legacy_store {
        Some(Arc::new(open_legacy_store(&data_dir.join("harness.db"))?))
    } else {
        None
    };
    let store = DurableStore::open(data_dir.join("rust-core.db"))
        .map_err(|error| runtime("durable_state_failed", &error.to_string()))?;
    let config = ServerConfig {
        secrets: configured_secrets(),
        legacy_store: legacy.clone(),
        // Content-addressed artifact blobs live beside the database (Python
        // `artifacts/` layout); enables the blob endpoints + upload paths.
        artifacts_dir: Some(data_dir.join("artifacts")),
        host,
        ..ServerConfig::default()
    };
    let (provider, model) = configured_provider(config.event_delay, true)?;
    let extraction_provider = provider.clone();
    let extraction_model = model.clone();
    let workspace = current_workspace()?;
    let (registry, extensions, plugin_store) = extension_registry(data_dir, legacy.clone()).await;
    let agent = AgentRuntime::new(provider, registry.clone());
    let mut server = AppServer::with_agent_runtime(
        config,
        store,
        agent,
        workspace,
        CapabilityPolicy::new(Some(Decision::Ask)),
        model,
    )
    .map_err(|error| runtime("durable_recovery_failed", &error.to_string()))?;
    if let (Some(extensions), Some(store)) = (extensions, plugin_store) {
        let extensions = Arc::new(extensions);
        server = server
            .with_run_lifecycle(Arc::new(CliExtensions(extensions.clone())))
            .with_extension_admin(Arc::new(CliExtensionAdmin {
                store,
                runtime: extensions,
            }));
    }
    // Application settings persist on the data root so a UI change survives a
    // restart instead of living in process memory.
    server = server.with_app_settings(Arc::new(FileAppSettings::new(
        data_dir.join("settings.json"),
    )));
    // The operator-owned global MCP admin (config store + live session registry)
    // is always available; plugin-bundled MCP servers stay on `extensions.status`.
    // Wired into the shared tool registry so connected servers' tools are live
    // in every agent run (Python `tool_bridge` parity).
    let mcp_admin = mcp_admin::CliMcpAdmin::new(data_dir).with_tool_registry(registry.clone());
    let mcp_admin = Arc::new(mcp_admin);
    server = server.with_mcp_admin(mcp_admin.clone());
    // Python `main.startup` parity: connect every enabled configured server and
    // register its tools before the first run is accepted. A store-read failure
    // degrades to no-registered-tools rather than aborting startup.
    if let Err(error) = mcp_admin.reconnect_all("local-user").await {
        eprintln!("mcp startup reconnect skipped: {error}");
    }
    // The operator-owned global skills store (a SKILL.md tree on the data root).
    server = server.with_skill_admin(Arc::new(skills_admin::CliSkillAdmin::new(data_dir)));
    // Executor-bound tools (deep_research / spawn_subagent / skills) need the
    // server-owned executors, so they are registered after the server exists.
    // ToolRegistry clones share the backing map — the live runtime sees them.
    if let Some(store) = legacy.clone() {
        for tool in executor_tools::executor_tool_registry(
            store,
            server.subagent_executor(),
            server.research_executor(),
            server.skill_admin(),
        ) {
            // `extend` builds a new registry; `register` inserts into the
            // shared map the live runtime already reads.
            if let Err(error) = registry.register(tool) {
                eprintln!("executor tool skipped: {error}");
            }
        }
    }
    // The live provider model-list probe (uses the stored provider rows + keyring).
    server = server.with_provider_probe(Arc::new(provider_probe::CliProviderProbe::new(
        legacy.clone(),
        configured_secrets(),
    )));
    // The forced RSS feed fetch/parse (pinned egress + legacy RSS store writes).
    server = server.with_rss_feed_fetch(Arc::new(rss_feed::CliRssFeedFetch::new(legacy.clone())));
    // LLM memory extraction (the configured provider + legacy memory store).
    server = server.with_memory_extractor(Arc::new(memory_extract::CliMemoryExtractor::new(
        legacy,
        extraction_provider,
        extraction_model,
    )));
    if let Some(executor) = server.task_executor() {
        executor.spawn_loop(std::time::Duration::from_secs(15));
    }
    Ok(server)
}

async fn extension_registry(
    data_dir: &std::path::Path,
    legacy_store: Option<Arc<LegacyStore>>,
) -> (
    cool_agent::ToolRegistry,
    Option<ExtensionRuntime>,
    Option<PluginStore>,
) {
    let mut registry = builtin_registry();
    // Store-backed parity tools exist only when the CLI serves a legacy store.
    if let Some(store) = legacy_store
        && let Ok(tools) = store_tools::store_tool_registry(store)
    {
        registry = registry.extend(tools).unwrap_or(registry);
    }
    // Web tools are env-configured (SEARCH_PROVIDER/SEARCH keys/allowlist) and
    // always registered — an unset provider errors gracefully inside the tool.
    registry = registry
        .extend(cool_agent::web_tool_registry(
            cool_agent::WebToolsConfig::from_env(),
        ))
        .unwrap_or(registry);
    let Ok(store) = PluginStore::open(data_dir.join("plugins")) else {
        return (registry, None, None);
    };
    let runtime = ExtensionRuntime::from_store(&store).ok();
    if let Some(runtime) = &runtime {
        start_configured_worker(runtime, CompatibilityAdapter::Codex, "COOL_CODEX_WORKER").await;
        start_configured_worker(runtime, CompatibilityAdapter::Claude, "COOL_CLAUDE_WORKER").await;
        start_configured_opencode_worker(runtime, data_dir).await;
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
        return (registry, runtime, Some(store));
    };
    for bundle in entries.into_iter().filter_map(|(_, result)| result.ok()) {
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
    (registry, runtime, Some(store))
}

fn current_workspace() -> Result<Workspace, (i32, serde_json::Value)> {
    Workspace::new(
        env::current_dir().map_err(|error| runtime("workspace_failed", &error.to_string()))?,
    )
    .map_err(|error| runtime("workspace_failed", &error.to_string()))
}

struct CliExtensions(Arc<ExtensionRuntime>);

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

/// Extension admin over the installed plugin store. Reads never mutate the store
/// and never expose a secret; mutations are the two narrow state changes the M8
/// review UI needs (enable/disable a plugin, approve/reject a hook) and append an
/// audit line. Neither can widen a capability, reach the DB/auth store or touch
/// the worker supervisor.
struct CliExtensionAdmin {
    store: PluginStore,
    runtime: Arc<ExtensionRuntime>,
}

impl CliExtensionAdmin {
    /// Best-effort admin audit: a failed append never masks the mutation result.
    /// The write goes through the store's append lock and is documented as a
    /// residual (it is not transactional with the mutation).
    fn audit(&self, record: serde_json::Value) {
        let _ = self.store.append_admin_audit(record);
    }

    /// Audits a rejected review attempt (an invalid target or a stale trust
    /// hash) and returns the message the caller sees, so every attempt — not
    /// only a state change — is recorded.
    fn audit_reject(
        &self,
        actor: &str,
        plugin: &str,
        hook: &str,
        outcome: &str,
        message: impl Into<String>,
    ) -> String {
        self.audit(json!({
            "actor": actor,
            "action": "hook_review",
            "plugin": plugin,
            "hook": hook,
            "outcome": outcome,
        }));
        message.into()
    }
}

#[async_trait]
impl ExtensionAdmin for CliExtensionAdmin {
    async fn status(&self) -> Result<ExtensionStatusResult, String> {
        extension_status(&self.store, &self.runtime).await
    }

    async fn set_plugin_enabled(
        &self,
        actor: &str,
        plugin: &str,
        enabled: bool,
    ) -> Result<PluginRecord, String> {
        // `PluginStore::set_enabled` fails closed when enabling a tree whose
        // content hash no longer matches.
        let entry = match self.store.set_enabled(plugin, enabled) {
            Ok(entry) => entry,
            Err(error) => {
                self.audit(json!({
                    "actor": actor,
                    "action": "plugin_enabled",
                    "plugin": plugin,
                    "enabled": enabled,
                    "outcome": "failed",
                }));
                return Err(error.to_string());
            }
        };
        self.audit(json!({
            "actor": actor,
            "action": "plugin_enabled",
            "plugin": plugin,
            "enabled": enabled,
            "outcome": "ok",
        }));
        let bundle = load_bundle(&entry);
        Ok(plugin_record(
            &entry,
            content_verified(&entry, bundle.as_ref()),
        ))
    }

    async fn set_hook_review(
        &self,
        actor: &str,
        plugin: &str,
        hook: &str,
        trust_hash: &str,
        approved: bool,
    ) -> Result<HookRecord, String> {
        let entry = match self.store.get(plugin) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                return Err(self.audit_reject(
                    actor,
                    plugin,
                    hook,
                    "plugin_not_found",
                    format!("plugin is not installed: {plugin}"),
                ));
            }
            Err(error) => {
                return Err(self.audit_reject(
                    actor,
                    plugin,
                    hook,
                    "store_error",
                    error.to_string(),
                ));
            }
        };
        let Some(bundle) = load_bundle(&entry) else {
            return Err(self.audit_reject(
                actor,
                plugin,
                hook,
                "plugin_not_loadable",
                format!("plugin is not loadable: {plugin}"),
            ));
        };
        let Some(declaration) = bundle.hooks.iter().find(|item| item.id == hook) else {
            return Err(self.audit_reject(
                actor,
                plugin,
                hook,
                "hook_not_declared",
                format!("hook is not declared: {plugin}/{hook}"),
            ));
        };
        // The submitted hash must match the declaration the operator reviewed,
        // or a stale approval could be replayed onto a changed definition.
        if declaration.trust_hash != trust_hash {
            return Err(self.audit_reject(
                actor,
                plugin,
                hook,
                "trust_mismatch",
                format!(
                    "hook trust hash does not match the reviewed definition for {plugin}/{hook}"
                ),
            ));
        }
        let result = if approved {
            self.store.set_hook_review(plugin, hook, trust_hash)
        } else {
            self.store.clear_hook_review(plugin, hook)
        };
        if let Err(error) = result {
            self.audit(json!({
                "actor": actor,
                "action": "hook_review",
                "plugin": plugin,
                "hook": hook,
                "approved": approved,
                "outcome": "failed",
            }));
            return Err(error.to_string());
        }
        self.audit(json!({
            "actor": actor,
            "action": "hook_review",
            "plugin": plugin,
            "hook": hook,
            "trustHash": trust_hash,
            "approved": approved,
            "outcome": "ok",
        }));
        Ok(hook_record(plugin, declaration, approved))
    }
}

fn load_bundle(entry: &InstalledPlugin) -> Option<PluginBundle> {
    PluginLoader
        .load(
            std::path::Path::new(&entry.install_path),
            std::path::Path::new(&entry.data_path),
        )
        .ok()
}

/// Same integrity rule as `PluginStore::load_entry`: a plugin is only
/// "verified" when its tree still hashes to the recorded content hash, its
/// manifest still has the recorded identity, and the bundle carries no
/// blockers — `loadable()` is what `load_entry` gates on. The runtime refuses
/// to load enabled plugins that fail this, so the admin must not project
/// their hooks/skills/MCP servers.
fn content_verified(entry: &InstalledPlugin, bundle: Option<&PluginBundle>) -> bool {
    bundle.is_some_and(|bundle| {
        bundle.loadable()
            && bundle.content_hash == entry.content_hash
            && bundle
                .manifest
                .as_ref()
                .is_some_and(|manifest| manifest.name == entry.name)
    })
}

fn plugin_record(entry: &InstalledPlugin, content_verified: bool) -> PluginRecord {
    PluginRecord {
        name: entry.name.clone(),
        version: entry.version.clone(),
        enabled: entry.enabled,
        source_type: entry.source_type.clone(),
        source: redact_location(&entry.source),
        revision: entry.revision.clone(),
        content_hash: entry.content_hash.clone(),
        installed_at: entry.installed_at.clone(),
        required_capabilities: entry.required_capabilities.clone(),
        resolved_dependencies: entry.resolved_dependencies.clone(),
        diagnostics: entry
            .diagnostics
            .iter()
            .map(|diagnostic| ExtensionDiagnosticRecord {
                code: diagnostic.get("code").cloned().unwrap_or_default(),
                level: diagnostic.get("level").cloned().unwrap_or_default(),
                message: diagnostic.get("message").cloned().unwrap_or_default(),
                path: diagnostic.get("path").cloned().unwrap_or_default(),
            })
            .collect(),
        content_verified,
    }
}

fn hook_record(plugin: &str, hook: &HookDeclaration, approved: bool) -> HookRecord {
    let handler = match &hook.handler {
        cool_extensions::HookHandler::Command { command, .. } => {
            format!("command:{}", command.to_string_lossy())
        }
        cool_extensions::HookHandler::Mcp { server, tool, .. } => {
            format!("mcp:{server}/{tool}")
        }
    };
    HookRecord {
        plugin: plugin.to_owned(),
        id: hook.id.clone(),
        event: hook.event.clone(),
        handler,
        order: hook.order,
        parallel: hook.parallel,
        capabilities: hook
            .capabilities
            .iter()
            .map(|value| value.as_str().to_owned())
            .collect(),
        trust_hash: hook.trust_hash.clone(),
        approved,
    }
}

async fn extension_status(
    store: &PluginStore,
    runtime: &ExtensionRuntime,
) -> Result<ExtensionStatusResult, String> {
    let entries = store.list().map_err(|error| error.to_string())?;
    let mut plugins = Vec::with_capacity(entries.len());
    let mut hooks = Vec::new();
    let mut skills = Vec::new();
    let mut mcp_servers = Vec::new();
    for entry in &entries {
        let bundle = load_bundle(entry);
        let verified = content_verified(entry, bundle.as_ref());
        plugins.push(plugin_record(entry, verified));
        if !entry.enabled || !verified {
            continue;
        }
        let Some(bundle) = bundle else { continue };
        let reviewed = store.reviewed_hook_hashes(&entry.name).unwrap_or_default();
        for hook in &bundle.hooks {
            let approved = reviewed
                .get(&hook.id)
                .is_some_and(|hash| hash == &hook.trust_hash);
            hooks.push(hook_record(&entry.name, hook, approved));
        }
        for skill in &bundle.skills {
            skills.push(SkillRecord {
                name: skill.name.clone(),
                description: skill.description.clone(),
                plugin: entry.name.clone(),
                allowed_tools: skill.allowed_tools.clone(),
            });
        }
        for server in &bundle.mcp_servers {
            let (transport, endpoint) = match server {
                cool_extensions::McpServer::Stdio { command, .. } => {
                    ("stdio", redact_location(&command.to_string_lossy()))
                }
                cool_extensions::McpServer::StreamableHttp { url, .. } => {
                    ("streamable_http", redact_location(url))
                }
            };
            mcp_servers.push(McpServerRecord {
                plugin: entry.name.clone(),
                name: server.name().to_owned(),
                transport: transport.to_owned(),
                endpoint,
            });
        }
    }
    // The last lifecycle event for a worker id wins (started/restarted report
    // it running, failed reports the crash), matching `status.get`.
    let mut workers: BTreeMap<String, WorkerRecord> = BTreeMap::new();
    for event in runtime.status_events().await {
        match event {
            CanonicalEvent::WorkerStarted(worker) | CanonicalEvent::WorkerRestarted(worker) => {
                workers.insert(
                    worker.worker_id.clone(),
                    WorkerRecord {
                        id: worker.worker_id,
                        status: "running".to_owned(),
                        attempt: i64::from(worker.attempt),
                        code: worker.code,
                    },
                );
            }
            CanonicalEvent::WorkerFailed(worker) => {
                workers.insert(
                    worker.worker_id.clone(),
                    WorkerRecord {
                        id: worker.worker_id,
                        status: "failed".to_owned(),
                        attempt: i64::from(worker.attempt),
                        code: worker.code,
                    },
                );
            }
            _ => {}
        }
    }
    Ok(ExtensionStatusResult {
        plugins,
        workers: workers.into_values().collect(),
        hooks,
        skills,
        mcp_servers,
        mcp_tool_policy: load_mcp_tool_policy(store.root()),
    })
}

/// Reads the MCP tool policy the extension registry applies to plugin-bundled
/// tools. A *missing* file means "no policy" (allow non-disabled tools), exactly
/// like the runtime; a present-but-unreadable or corrupt file fails closed and
/// is projected as an explicit deny-all so the UI can see the degraded state.
fn load_mcp_tool_policy(root: &std::path::Path) -> McpToolPolicyRecord {
    let path = root.join("mcp-tool-policy.json");
    if !path.exists() {
        return McpToolPolicyRecord::default();
    }
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<McpToolPolicy>(&bytes) {
            Ok(policy) => McpToolPolicyRecord {
                enabled: policy.enabled.map(|items| items.into_iter().collect()),
                disabled: policy.disabled.into_iter().collect(),
            },
            Err(_) => deny_all_policy(),
        },
        Err(_) => deny_all_policy(),
    }
}

fn deny_all_policy() -> McpToolPolicyRecord {
    McpToolPolicyRecord {
        enabled: Some(Vec::new()),
        disabled: Vec::new(),
    }
}

/// Redacts a location string before it reaches the web admin surface. A URL's
/// userinfo, query and fragment can carry a token (a Git source may embed
/// credentials; an MCP HTTP endpoint may carry an access token in a query
/// string), so they are stripped; any remaining secret-shaped text is masked.
fn redact_location(value: &str) -> String {
    if let Ok(mut url) = url::Url::parse(value) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return mask_secrets(url.as_str());
    }
    mask_secrets(value)
}

/// File-backed application settings on the data root. One JSON document so the
/// surface can grow; today it holds the default system prompt. Writes are
/// atomic (unique temp + rename); the prompt is the user's own config, so it is
/// stored as-is and never logged (it is user content, not a secret to mask).
///
/// The document rejects unknown fields, so a typo fails closed instead of being
/// silently ignored; adding a second setting later is a deliberate
/// read-compatible migration, not an automatic one.
struct FileAppSettings {
    path: std::path::PathBuf,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct SettingsDocument {
    system_prompt: String,
}

/// Bounds the persisted prompt so one setting cannot grow the settings file (or
/// the settings frame) without limit.
const MAX_SYSTEM_PROMPT_CHARS: usize = 100_000;

impl FileAppSettings {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn read_document(&self) -> Result<SettingsDocument, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<SettingsDocument>(&bytes)
                .map_err(|error| format!("settings file is invalid: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(SettingsDocument::default())
            }
            Err(error) => Err(format!("settings file could not be read: {error}")),
        }
    }

    fn write_document(&self, document: &SettingsDocument) -> Result<(), String> {
        let payload = serde_json::to_vec_pretty(document)
            .map_err(|error| format!("settings could not be encoded: {error}"))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("settings directory could not be created: {error}"))?;
        }
        // A unique temp name (pid + nanos) keeps concurrent writers from tearing
        // each other's temp file; the rename is the atomic commit.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = self
            .path
            .with_extension(format!("{}.{nanos}.tmp", std::process::id()));
        std::fs::write(&temporary, payload)
            .map_err(|error| format!("settings could not be written: {error}"))?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("settings could not be replaced: {error}"))
    }
}

fn system_prompt_record(prompt: String) -> SystemPromptRecord {
    let is_custom = !prompt.trim().is_empty();
    SystemPromptRecord {
        prompt,
        is_custom,
        source: if is_custom { "inline" } else { "builtin" }.to_owned(),
    }
}

#[async_trait]
impl AppSettings for FileAppSettings {
    async fn system_prompt(&self) -> Result<SystemPromptRecord, String> {
        Ok(system_prompt_record(self.read_document()?.system_prompt))
    }

    async fn set_system_prompt(&self, prompt: &str) -> Result<SystemPromptRecord, String> {
        if prompt.chars().count() > MAX_SYSTEM_PROMPT_CHARS {
            return Err(format!(
                "system prompt is too long (max {MAX_SYSTEM_PROMPT_CHARS} characters)"
            ));
        }
        let stored = if prompt.trim().is_empty() {
            String::new()
        } else {
            prompt.to_owned()
        };
        self.write_document(&SettingsDocument {
            system_prompt: stored.clone(),
        })?;
        Ok(system_prompt_record(stored))
    }
}

/// `cool serve`: the browser-facing HTTP/SSE facade over the Rust runtime.
///
/// The App Protocol remains the single business boundary: `/api/rpc` carries
/// canonical `RpcRequest`/`ServerFrame` JSON and `/api/events` is the canonical
/// cursor/reconnect event stream. Static React assets are served from
/// `--assets` with an SPA fallback.
async fn serve_command(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    let mut options = cool_http::ServeOptions::default();
    let mut data_dir = default_data_dir();
    let mut legacy_store = false;
    let mut process_launcher: Option<String> = None;
    let mut sandbox: Option<String> = None;
    let mut allow_shell = false;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--data-dir" => {
                data_dir = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing data directory"))?,
                );
            }
            "--bind" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| usage("missing bind address"))?;
                let address: std::net::IpAddr = value
                    .parse()
                    .map_err(|_| usage("bind must be an IP address"))?;
                options.bind.set_ip(address);
            }
            "--port" => {
                let value = arguments.next().ok_or_else(|| usage("missing port"))?;
                let port: u16 = value.parse().map_err(|_| usage("port must be a number"))?;
                options.bind.set_port(port);
            }
            "--assets" => {
                options.assets = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing assets directory"))?,
                ));
            }
            "--token" => {
                options.token = Some(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing token value"))?,
                );
            }
            "--public-url" => {
                options.public_url = Some(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing public URL value"))?,
                );
            }
            "--profile" => {
                let value = arguments.next().ok_or_else(|| usage("missing profile"))?;
                options.profile = match value.as_str() {
                    "local" => cool_http::ServeProfile::Local,
                    "server" => cool_http::ServeProfile::Server,
                    _ => return Err(usage("profile must be local or server")),
                };
            }
            "--trusted-proxy" => options.trust_proxy = true,
            "--tls-terminated" => options.tls_terminated = true,
            "--allow-remote" => options.allow_remote = true,
            "--legacy-store" => legacy_store = true,
            "--process-launcher" => {
                process_launcher = Some(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing process-launcher value"))?,
                );
            }
            "--sandbox" => {
                sandbox = Some(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing sandbox value"))?,
                );
            }
            "--allow-shell" => allow_shell = true,
            _ => return Err(usage("unknown serve argument")),
        }
    }
    if options.token.is_none()
        && let Ok(token) = env::var("COOL_API_TOKEN")
        && !token.is_empty()
    {
        options.token = Some(token);
    }
    // Validate the deployment profile before any startup side effect:
    // `build_server` opens/creates stores and spawns the scheduler loop.
    cool_http::validate_options(&options).map_err(|error| usage(&error.to_string()))?;
    let bind = options.bind;
    let profile = cool_http::profile_name(options.profile);
    let host = cli_host(process_launcher, sandbox, allow_shell).map_err(|error| usage(&error))?;
    let server = build_server(&data_dir, legacy_store, host).await?;
    let facade =
        cool_http::HttpFacade::new(server, options).map_err(|error| usage(&error.to_string()))?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| runtime("serve_bind_failed", &error.to_string()))?;
    let address = listener
        .local_addr()
        .map_err(|error| runtime("serve_bind_failed", &error.to_string()))?;
    eprintln!("cool serve: profile={profile} listening on http://{address}");
    facade
        .serve(listener)
        .await
        .map_err(|error| runtime("serve_failed", &error.to_string()))
}

/// Explicit, operator-driven adoption of the legacy Python database.
///
/// `cool serve`/`app-server` deliberately open a Python-owned `harness.db`
/// read-only (adoption is a migration decision, not a startup side effect). This
/// command performs that decision: it opens the baseline store writable, which
/// takes a verified backup before the first Rust write and records the Rust
/// migration owner, then prints the adoption report. Re-running it is
/// idempotent (no second backup) and a database at another Alembic revision
/// fails closed.
async fn store_command(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    let mut action: Option<String> = None;
    let mut data_dir = default_data_dir();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "adopt" => action = Some("adopt".to_owned()),
            "--data-dir" => {
                data_dir = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing data directory"))?,
                );
            }
            _ => return Err(usage("unknown store argument")),
        }
    }
    match action.as_deref() {
        Some("adopt") => {}
        _ => return Err(usage("store needs an action: adopt")),
    }
    let database = data_dir.join("harness.db");
    if !database.exists() {
        return Err(runtime(
            "legacy_store_missing",
            "harness.db was not found under the data directory",
        ));
    }
    let store = cool_store::LegacyStore::open(&database, &cool_store::StoreOptions::default())
        .map_err(|error| runtime("store_adopt_failed", &error.to_string()))?;
    let report = store.adoption_report();
    let (adopted, revision, backup) = match report {
        Some(report) => (
            report.adopted,
            report.adopted_revision.clone(),
            report
                .backup
                .as_ref()
                .map(|backup| backup.path.to_string_lossy().into_owned()),
        ),
        None => (false, String::new(), None),
    };
    print_json(&json!({
        "status": "ok",
        "database": database.to_string_lossy(),
        "adopted": adopted,
        "alembicRevision": revision,
        "schemaVersion": store.schema_version().ok(),
        "rustOwned": store.is_rust_owned().unwrap_or(false),
        "backupPath": backup,
    }))?;
    Ok(())
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
            "plugin command is: plugin install|update|remove|enable|disable|list|validate|doctor [argument]",
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
        "update" => {
            let (Some(name), Some(source)) = (arguments.get(1), arguments.get(2)) else {
                return Err(usage("plugin update needs a name and a source path"));
            };
            let store = open_plugin_store(&data_dir)?;
            let entry = store
                .update_local(name, std::path::Path::new(source))
                .map_err(|error| runtime("plugin_update_failed", &error.to_string()))?;
            print_json(&plugin_entry_json(&entry))
        }
        "remove" | "uninstall" => {
            let Some(name) = arguments.get(1) else {
                return Err(usage("plugin remove needs a name"));
            };
            let store = open_plugin_store(&data_dir)?;
            let entry = store
                .uninstall(name)
                .map_err(|error| runtime("plugin_remove_failed", &error.to_string()))?;
            print_json(&json!({"removed": plugin_entry_json(&entry)}))
        }
        "enable" | "disable" => {
            let Some(name) = arguments.get(1) else {
                return Err(usage("plugin enable|disable needs a name"));
            };
            let store = open_plugin_store(&data_dir)?;
            let entry = store
                .set_enabled(name, action == "enable")
                .map_err(|error| runtime("plugin_enable_failed", &error.to_string()))?;
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
            let transparency = match store.verify_transparency_log() {
                Ok(entries) => json!({"valid": true, "entries": entries}),
                Err(error) => json!({"valid": false, "error": error.to_string()}),
            };
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
                    "publisher": entry.publisher,
                    "signatureStatus": entry.signature_status,
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
            print_json(&json!({"plugins": reports, "transparency": transparency}))
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
        for bundle in bundles.into_iter().filter_map(|(_, result)| result.ok()) {
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
        for bundle in bundles.into_iter().filter_map(|(_, result)| result.ok()) {
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
        "publisher": entry.publisher,
        "signatureStatus": entry.signature_status,
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

/// Resolved OpenCode worker configuration from the process environment.
struct OpenCodeWorkerEnv {
    entry: PathBuf,
    bun: PathBuf,
    root: PathBuf,
    data: PathBuf,
    workspace: Option<PathBuf>,
}

/// Pure env-resolution for the OpenCode worker so the base install stays
/// unchanged: no `COOL_OPENCODE_WORKER` entry means no worker at all. `bun`
/// defaults to the bare name (resolved through `PATH` at spawn time), the
/// install root defaults to the entry's directory, and the writable data root
/// defaults to `<fallback_data>` (under the plugin store's data area but outside
/// any installation root). A workspace is granted only when the operator opts
/// in with `COOL_OPENCODE_WORKSPACE`, so the default never hands the plugin the
/// tree that contains its own installation.
fn opencode_worker_config(
    entry: Option<PathBuf>,
    bun: Option<PathBuf>,
    root: Option<PathBuf>,
    data: Option<PathBuf>,
    workspace: Option<PathBuf>,
    fallback_data: &std::path::Path,
) -> Option<OpenCodeWorkerEnv> {
    let entry = entry?;
    let bun = bun.unwrap_or_else(|| PathBuf::from("bun"));
    let root = root.or_else(|| entry.parent().map(std::path::Path::to_path_buf))?;
    let data = data.unwrap_or_else(|| fallback_data.to_path_buf());
    Some(OpenCodeWorkerEnv {
        entry,
        bun,
        root,
        data,
        workspace,
    })
}

/// Starts the experimental OpenCode Bun worker when `COOL_OPENCODE_WORKER`
/// points at an executable plugin entry. A missing Bun, an invalid plugin tree
/// or a denied capability is reported as a plugin status and never aborts
/// startup, so the Python/Bun-free base install is unaffected.
async fn start_configured_opencode_worker(runtime: &ExtensionRuntime, data_dir: &std::path::Path) {
    let Some(config) = opencode_worker_config(
        env::var_os("COOL_OPENCODE_WORKER").map(PathBuf::from),
        env::var_os("COOL_BUN_PATH").map(PathBuf::from),
        env::var_os("COOL_OPENCODE_ROOT").map(PathBuf::from),
        env::var_os("COOL_OPENCODE_DATA").map(PathBuf::from),
        env::var_os("COOL_OPENCODE_WORKSPACE").map(PathBuf::from),
        &data_dir.join("plugins").join("opencode-data"),
    ) else {
        return;
    };
    let core = CapabilityPolicy::new(Some(Decision::Ask));
    let spec = OpenCodeWorkerConfig {
        bun: config.bun,
        plugin_root: config.root,
        plugin_data: config.data,
        entry: config.entry,
        granted_workspaces: config.workspace.into_iter().collect(),
        environment: BTreeMap::new(),
        required_capabilities: BTreeSet::new(),
    };
    match opencode_launch_spec(spec, &core) {
        Ok(spec) => {
            let _ = runtime
                .start_worker(CompatibilityAdapter::OpenCode, spec)
                .await;
        }
        Err(error) => {
            runtime
                .report_plugin_status(
                    "core/opencode-worker",
                    "failed",
                    Some(format!("opencode_worker: {error}")),
                )
                .await;
        }
    }
}

async fn run_prompt(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    let mut scripted = false;
    let mut allow_shell = false;
    let mut process_launcher: Option<String> = None;
    let mut sandbox: Option<String> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--scripted" => scripted = true,
            "--allow-shell" => allow_shell = true,
            "--process-launcher" => {
                process_launcher = Some(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing process-launcher value"))?,
                );
            }
            "--sandbox" => {
                sandbox = Some(
                    arguments
                        .next()
                        .ok_or_else(|| usage("missing sandbox value"))?,
                );
            }
            _ if argument.starts_with("--") => {
                return Err(usage("unknown run argument"));
            }
            _ => prompt_parts.push(argument),
        }
    }
    if prompt_parts.is_empty() {
        return Err(usage("run needs a prompt"));
    }
    let host = cli_host(process_launcher, sandbox, allow_shell).map_err(|error| usage(&error))?;
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
                mode: None,
                temperature: 0.7,
                max_tokens: None,
                limits: AgentLimits::default(),
                tool_names: None,
                tool_context: ToolContext::new(
                    workspace,
                    CapabilityPolicy::new(Some(Decision::Ask)),
                )
                .with_launcher(host.launcher.clone())
                .with_environment(host.environment.clone()),
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
    let provider_kind = env::var("COOL_PROVIDER").unwrap_or_default().to_lowercase();
    match provider_kind.as_str() {
        "anthropic" => return configured_anthropic_provider(allow_scripted_fallback),
        "" | "openai" | "openai-compatible" | "openai_compatible" => {}
        // An explicit but unknown provider must fail closed — never silently
        // fall back to a different backend than the operator asked for.
        other => {
            return Err(runtime(
                "provider_config_invalid",
                &format!("unknown COOL_PROVIDER: {other}"),
            ));
        }
    }
    // With no explicit provider, a stray ANTHROPIC_API_KEY selects the
    // Anthropic-native driver; the OpenAI-compatible path stays the default.
    if provider_kind.is_empty()
        && env::var("ANTHROPIC_API_KEY").is_ok_and(|value| !value.is_empty())
    {
        return configured_anthropic_provider(allow_scripted_fallback);
    }
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
            "COOL_PROVIDER=anthropic with ANTHROPIC_API_KEY, or OPENAI_API_KEY or an explicit OPENAI_BASE_URL is required; use --scripted only for deterministic local checks",
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
        .or_else(|_| env::var("COOL_MODEL"))
        .unwrap_or_else(|_| "gpt-5-mini".to_owned());
    Ok((Arc::new(provider), model))
}

/// Anthropic-native provider wiring (parity with `providers/anthropic.py`):
/// `ANTHROPIC_API_KEY` + optional `ANTHROPIC_BASE_URL`, model from
/// `ANTHROPIC_MODEL`/`COOL_MODEL` or the current Claude default.
fn configured_anthropic_provider(
    allow_scripted_fallback: bool,
) -> Result<(Arc<dyn ModelDriver>, String), (i32, serde_json::Value)> {
    let api_key = env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        if allow_scripted_fallback {
            return Ok((
                Arc::new(ScriptedDriver::echo_with_delay(std::time::Duration::ZERO)),
                "scripted-echo".to_owned(),
            ));
        }
        return Err(runtime(
            "provider_credentials_missing",
            "COOL_PROVIDER=anthropic requires ANTHROPIC_API_KEY",
        ));
    }
    let base_url = env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.anthropic.com".to_owned());
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
    let provider = AnthropicDriver::new(&base_url, api_key, policy)
        .map_err(|error| runtime("provider_config_invalid", &error.to_string()))?;
    let model = env::var("ANTHROPIC_MODEL")
        .or_else(|_| env::var("COOL_MODEL"))
        .unwrap_or_else(|_| "claude-sonnet-4-5".to_owned());
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
        "Cool Rust CLI\n\nCommands:\n  (no arguments)              interactive TUI\n  app-server [--transport stdio|local] [--endpoint PATH] [--data-dir PATH] [--legacy-store]\n  serve [--data-dir PATH] [--bind IP] [--port N] [--assets DIR] [--profile local|server]\n        [--token TOKEN] [--public-url URL] [--trusted-proxy] [--tls-terminated]\n        [--allow-remote] [--legacy-store]\n  run [--scripted] <prompt>\n  acp                         ACP v1 agent over stdio\n  plugin install <path|git-url> [--revision SHA]\n  plugin list\n  plugin validate <path>\n  plugin doctor [path]\n  store adopt [--data-dir PATH]\n  mcp list\n  hooks list\n  doctor [--data-dir PATH]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_worker_is_absent_without_the_env_entry() {
        let fallback = std::path::Path::new("/data/plugins/opencode-data");
        assert!(
            opencode_worker_config(None, None, None, None, None, fallback).is_none(),
            "the base install must not start an OpenCode worker without COOL_OPENCODE_WORKER"
        );
    }

    #[test]
    fn opencode_worker_defaults_bun_root_and_data() {
        let fallback = std::path::Path::new("/data/plugins/opencode-data");
        let config = opencode_worker_config(
            Some(PathBuf::from("/plugins/demo/lib/index.ts")),
            None,
            None,
            None,
            None,
            fallback,
        )
        .expect("entry configured");
        assert_eq!(config.bun, PathBuf::from("bun"));
        assert_eq!(config.root, PathBuf::from("/plugins/demo/lib"));
        assert_eq!(config.data, fallback.to_path_buf());
        assert!(
            config.workspace.is_none(),
            "a workspace must be opt-in via COOL_OPENCODE_WORKSPACE"
        );
    }

    fn write_admin_fixture(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("skills/demo")).unwrap();
        std::fs::create_dir_all(root.join("io.github.luckystrker.cool/hooks")).unwrap();
        std::fs::write(
            root.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"admin-demo","version":"1.2.0"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: Demo skill\n---\nDo the thing.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("mcp.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/mcp.schema.json","mcpServers":{"files":{"type":"stdio","command":"mcp-files"},"remote":{"type":"streamable-http","url":"https://mcp.example.com/rpc?token=secret-value"}}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("io.github.luckystrker.cool/hooks/hooks.json"),
            r#"{"version":1,"hooks":[{"id":"audit","event":"PreToolUse","handler":{"type":"command","command":"echo","args":["hi"]}}]}"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn extension_status_maps_the_installed_plugin_store() {
        let temporary = tempfile::tempdir().unwrap();
        let data_dir = temporary.path().join("data");
        let source = temporary.path().join("plugin-source");
        write_admin_fixture(&source);

        let store = PluginStore::open(data_dir.join("plugins")).unwrap();
        let installed = store.install_local(&source).unwrap();
        assert_eq!(installed.name, "admin-demo");
        // The store installs disabled; an admin snapshot must report that and
        // only enumerate skills/hooks/servers for enabled plugins.
        let disabled = extension_status(&store, &ExtensionRuntime::from_store(&store).unwrap())
            .await
            .unwrap();
        assert_eq!(disabled.plugins.len(), 1);
        assert!(!disabled.plugins[0].enabled);
        assert!(disabled.skills.is_empty());
        assert!(disabled.mcp_servers.is_empty());

        store.set_enabled("admin-demo", true).unwrap();
        let runtime = std::sync::Arc::new(ExtensionRuntime::from_store(&store).unwrap());
        let status = extension_status(&store, &runtime).await.unwrap();
        assert!(status.plugins[0].enabled);
        assert!(status.plugins[0].content_verified);
        assert_eq!(status.plugins[0].version, "1.2.0");
        assert_eq!(status.skills.len(), 1);
        assert_eq!(status.skills[0].name, "demo");
        assert_eq!(status.skills[0].plugin, "admin-demo");
        assert_eq!(status.mcp_servers.len(), 2);
        assert_eq!(status.mcp_servers[0].name, "files");
        assert_eq!(status.mcp_servers[0].transport, "stdio");
        assert_eq!(status.mcp_servers[0].endpoint, "mcp-files");
        assert_eq!(status.mcp_servers[1].name, "remote");
        assert_eq!(status.mcp_servers[1].transport, "streamable_http");
        assert_eq!(
            status.mcp_servers[1].endpoint, "https://mcp.example.com/rpc",
            "an endpoint query token must not reach the admin surface"
        );
        assert_eq!(status.hooks.len(), 1);
        assert_eq!(status.hooks[0].id, "audit");
        assert_eq!(status.hooks[0].event, "PreToolUse");
        assert!(!status.hooks[0].approved);
        assert!(!status.hooks[0].trust_hash.is_empty());
        assert!(status.mcp_tool_policy.enabled.is_none());
        assert!(status.mcp_tool_policy.disabled.is_empty());

        // Approving the exact trust hash flips the review state; a stale hash
        // must not.
        store
            .set_hook_review("admin-demo", "audit", "deadbeef")
            .unwrap();
        let stale = extension_status(&store, &runtime).await.unwrap();
        assert!(!stale.hooks[0].approved);
        let trust_hash = status.hooks[0].trust_hash.clone();
        store
            .set_hook_review("admin-demo", "audit", &trust_hash)
            .unwrap();
        let approved = extension_status(&store, &runtime).await.unwrap();
        assert!(approved.hooks[0].approved);

        // Durable admin mutations through the trait: enable/disable + review.
        let admin = CliExtensionAdmin {
            store: store.clone(),
            runtime: runtime.clone(),
        };
        let disabled = admin
            .set_plugin_enabled("tester", "admin-demo", false)
            .await
            .unwrap();
        assert!(!disabled.enabled && disabled.content_verified);
        let enabled = admin
            .set_plugin_enabled("tester", "admin-demo", true)
            .await
            .unwrap();
        assert!(enabled.enabled && enabled.content_verified);
        // A real-store enable/disable replay is idempotent.
        assert!(
            admin
                .set_plugin_enabled("tester", "admin-demo", true)
                .await
                .is_ok()
        );

        // Reject is idempotent: clearing an approved review twice succeeds and
        // leaves it unapproved.
        let rejected = admin
            .set_hook_review("tester", "admin-demo", "audit", &trust_hash, false)
            .await
            .unwrap();
        assert!(!rejected.approved);
        assert!(
            admin
                .set_hook_review("tester", "admin-demo", "audit", &trust_hash, false)
                .await
                .is_ok()
        );
        assert!(!extension_status(&store, &runtime).await.unwrap().hooks[0].approved);
        let reapproved = admin
            .set_hook_review("tester", "admin-demo", "audit", &trust_hash, true)
            .await
            .unwrap();
        assert!(reapproved.approved);
        assert!(extension_status(&store, &runtime).await.unwrap().hooks[0].approved);

        // A stale hash cannot be approved, and unknown targets fail closed.
        assert!(
            admin
                .set_hook_review("tester", "admin-demo", "audit", "deadbeef", true)
                .await
                .is_err()
        );
        assert!(
            admin
                .set_plugin_enabled("tester", "missing", true)
                .await
                .is_err()
        );
        assert!(
            admin
                .set_hook_review("tester", "admin-demo", "missing", &trust_hash, true)
                .await
                .is_err()
        );
        assert!(
            admin
                .set_hook_review("tester", "missing", "audit", &trust_hash, true)
                .await
                .is_err()
        );

        // Every attempt is recorded in the admin audit log with the actor.
        let audit =
            std::fs::read_to_string(store.root().join("extension-admin-audit.jsonl")).unwrap();
        assert!(audit.contains("\"actor\":\"tester\""));
        assert!(audit.contains("\"action\":\"plugin_enabled\""));
        assert!(audit.contains("\"action\":\"hook_review\""));
        assert!(audit.contains("\"outcome\":\"trust_mismatch\""));
        assert!(audit.contains("\"outcome\":\"plugin_not_found\""));
        assert!(audit.contains("\"outcome\":\"hook_not_declared\""));

        // A tampered tree (content no longer matches the recorded hash) is
        // reported as unverified, and its hooks/skills/servers are not
        // projected, matching the runtime's refusal to load it.
        let installed = store.get("admin-demo").unwrap().unwrap();
        std::fs::write(
            std::path::Path::new(&installed.install_path).join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: Tampered\n---\nChanged.\n",
        )
        .unwrap();
        let tampered = extension_status(&store, &runtime).await.unwrap();
        assert!(tampered.plugins[0].enabled);
        assert!(!tampered.plugins[0].content_verified);
        assert!(tampered.hooks.is_empty());
        assert!(tampered.skills.is_empty());
        assert!(tampered.mcp_servers.is_empty());

        // Enabling a tampered tree fails closed through the admin mutation and
        // the failure is audited.
        assert!(
            admin
                .set_plugin_enabled("tester", "admin-demo", true)
                .await
                .is_err()
        );
        let audit =
            std::fs::read_to_string(store.root().join("extension-admin-audit.jsonl")).unwrap();
        let tamper_failed = audit.lines().any(|line| {
            line.contains("\"plugin\":\"admin-demo\"") && line.contains("\"outcome\":\"failed\"")
        });
        assert!(
            tamper_failed,
            "the tamper failure must be audited distinctly: {audit}"
        );

        // A present-but-corrupt policy fails closed (deny-all projection),
        // unlike a missing file which means "no policy".
        std::fs::write(store.root().join("mcp-tool-policy.json"), b"{not json").unwrap();
        let corrupt = extension_status(&store, &runtime).await.unwrap();
        assert_eq!(corrupt.mcp_tool_policy.enabled, Some(Vec::new()));
        assert!(corrupt.mcp_tool_policy.disabled.is_empty());
    }

    #[tokio::test]
    async fn file_app_settings_round_trips_and_bounds() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("settings.json");
        let settings = FileAppSettings::new(path.clone());

        let record = settings.system_prompt().await.unwrap();
        assert_eq!(record.prompt, "");
        assert!(!record.is_custom);

        let record = settings.set_system_prompt("You are Cool.").await.unwrap();
        assert!(record.is_custom);
        assert_eq!(record.source, "inline");
        assert!(path.is_file(), "the settings file is written");
        assert_eq!(
            settings.system_prompt().await.unwrap().prompt,
            "You are Cool."
        );
        // Replaying the same write is idempotent.
        settings.set_system_prompt("You are Cool.").await.unwrap();
        assert_eq!(
            settings.system_prompt().await.unwrap().prompt,
            "You are Cool."
        );

        // A whitespace-only value clears the default.
        let record = settings.set_system_prompt("   ").await.unwrap();
        assert!(!record.is_custom);
        assert_eq!(record.source, "builtin");

        // A corrupt file fails closed and an oversized prompt is rejected.
        std::fs::write(&path, b"{not json").unwrap();
        assert!(settings.system_prompt().await.is_err());
        let oversized = "x".repeat(MAX_SYSTEM_PROMPT_CHARS + 1);
        assert!(settings.set_system_prompt(&oversized).await.is_err());
    }

    #[tokio::test]
    async fn store_tools_execute_against_a_legacy_store() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("harness.db");
        let store = Arc::new(open_legacy_store(&database).unwrap());
        let registry = builtin_registry()
            .extend(store_tools::store_tool_registry(store.clone()).unwrap())
            .unwrap();
        for name in [
            "memory_remember",
            "memory_recall",
            "memory_forget",
            "memory_update",
            "memory_list",
            "set_working_memory",
            "get_working_memory",
            "entity_lookup",
            "read_wiki",
            "write_wiki",
            "search_wiki",
            "update_wiki",
            "rss_list",
            "rss_subscribe",
            "rss_unsubscribe",
            "create_task",
            "list_tasks",
            "update_task",
            "delete_task",
            "parse_cron",
        ] {
            assert!(registry.get(name).is_some(), "{name} must be registered");
        }
        let context = ToolContext::new(
            Workspace::new(temporary.path()).unwrap(),
            CapabilityPolicy::new(Some(Decision::Allow)),
        )
        .with_actor("local-user");

        let remember = registry.get("memory_remember").unwrap();
        let result = remember
            .execute(
                &context,
                json!({"content": "the build uses Rust", "importance": 0.9}),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "remember failed: {:?}", result.output);
        // An agent-sourced write lands in pending_confirmation, matching Python,
        // so it is not in the active list until confirmed.
        assert_eq!(
            result.output.get("status").and_then(|value| value.as_str()),
            Some("pending_confirmation")
        );

        let list = registry.get("memory_list").unwrap();
        let listed = list.execute(&context, json!({})).await.unwrap();
        assert!(listed.output.as_array().unwrap().is_empty());

        // Seed an active (user_explicit) memory to exercise list/recall/update.
        let active = store
            .create_memory_item(
                "local-user",
                &cool_store::domains::memory::NewMemoryItem {
                    content: "the build uses Rust".to_owned(),
                    source: "user_explicit".to_owned(),
                    ..cool_store::domains::memory::NewMemoryItem::default()
                },
            )
            .unwrap();
        let id = active.id;

        let listed = list.execute(&context, json!({})).await.unwrap();
        assert_eq!(listed.output.as_array().unwrap().len(), 1);

        // Recall is a lexical filter over active memories, not semantic ranking.
        let recall = registry.get("memory_recall").unwrap();
        let hit = recall
            .execute(&context, json!({"query": "rust"}))
            .await
            .unwrap();
        assert_eq!(hit.output.as_array().unwrap().len(), 1);
        let miss = recall
            .execute(&context, json!({"query": "python"}))
            .await
            .unwrap();
        assert!(miss.output.as_array().unwrap().is_empty());

        let update = registry.get("memory_update").unwrap();
        let updated = update
            .execute(
                &context,
                json!({"memory_id": id, "content": "the build uses Rust 2024"}),
            )
            .await
            .unwrap();
        assert_eq!(
            updated
                .output
                .get("content")
                .and_then(|value| value.as_str()),
            Some("the build uses Rust 2024")
        );

        // An agent-sourced update is capped at the agent importance maximum.
        let clamped = update
            .execute(&context, json!({"memory_id": id, "importance": 1.0}))
            .await
            .unwrap();
        assert_eq!(
            clamped
                .output
                .get("importance")
                .and_then(|value| value.as_f64()),
            Some(0.9)
        );

        // A soft forget archives, so the active list drops it.
        let forget = registry.get("memory_forget").unwrap();
        let forgotten = forget
            .execute(&context, json!({"memory_id": id}))
            .await
            .unwrap();
        assert!(!forgotten.is_error);
        let listed = list.execute(&context, json!({})).await.unwrap();
        assert!(listed.output.as_array().unwrap().is_empty());

        // A hard forget removes the row entirely.
        let hard = forget
            .execute(&context, json!({"memory_id": id, "hard": true}))
            .await
            .unwrap();
        assert!(!hard.is_error);
        assert!(store.get_memory_item("local-user", id).is_err());

        // Malformed input is rejected and an empty entity table is handled.
        assert!(
            remember
                .execute(&context, json!({"bogus": 1}))
                .await
                .is_err()
        );
        assert!(
            remember
                .execute(&context, json!("not an object"))
                .await
                .is_err()
        );
        assert!(
            remember
                .execute(&context, json!({"content": ""}))
                .await
                .is_err()
        );
        let lookup = registry.get("entity_lookup").unwrap();
        let entities = lookup
            .execute(&context, json!({"query": "anything"}))
            .await
            .unwrap();
        assert!(entities.output.as_array().unwrap().is_empty());

        // Working memory fails closed without a bound conversation.
        let set_wm = registry.get("set_working_memory").unwrap();
        assert!(
            set_wm
                .execute(&context, json!({"key": "k", "value": "v"}))
                .await
                .unwrap()
                .is_error
        );
        // With a bound conversation the scratchpad merges JSON values.
        let conversation = store
            .create_conversation(
                "local-user",
                &cool_store::domains::conversations::NewConversation::default(),
            )
            .unwrap();
        let bound = context.clone().with_conversation(Some(conversation.id));
        // Seed a compaction summary so a set must preserve it (not replace the row).
        store
            .upsert_working_memory(
                "local-user",
                conversation.id,
                &json!({"seed": true}),
                Some("prior summary"),
                Some(7),
                Some(42),
            )
            .unwrap();
        let set = set_wm
            .execute(&bound, json!({"key": "goal", "value": "{\"step\": 2}"}))
            .await
            .unwrap();
        assert!(!set.is_error, "set_working_memory failed: {:?}", set.output);
        // Setting a second key merges rather than replacing the whole state.
        let second = set_wm
            .execute(&bound, json!({"key": "hypothesis", "value": "cache miss"}))
            .await
            .unwrap();
        assert!(!second.is_error);
        let get_wm = registry.get("get_working_memory").unwrap();
        let value = get_wm
            .execute(&bound, json!({"key": "goal"}))
            .await
            .unwrap();
        assert_eq!(
            value
                .output
                .get("value")
                .and_then(|value| value.get("step"))
                .and_then(serde_json::Value::as_i64),
            Some(2)
        );
        let all = get_wm.execute(&bound, json!({})).await.unwrap();
        assert_eq!(
            all.output
                .get("goal")
                .and_then(|value| value.get("step"))
                .and_then(serde_json::Value::as_i64),
            Some(2)
        );
        assert_eq!(
            all.output
                .get("hypothesis")
                .and_then(serde_json::Value::as_str),
            Some("cache miss")
        );
        assert_eq!(
            all.output.get("seed").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        // The compaction fields survive a scratchpad write.
        let row = store
            .get_working_memory("local-user", conversation.id)
            .unwrap()
            .unwrap();
        assert_eq!(row.summary.as_deref(), Some("prior summary"));
        assert_eq!(row.summary_up_to_message_id, Some(7));
        assert_eq!(row.token_estimate, Some(42));

        // Wiki round-trip: write, read by id, search, update, read.
        let write_wiki = registry.get("write_wiki").unwrap();
        let created = write_wiki
            .execute(
                &context,
                json!({"title": "Rust notes", "content": "# Notes\nmemory tools", "category": "project"}),
            )
            .await
            .unwrap();
        assert!(!created.is_error, "write_wiki failed: {:?}", created.output);
        let article_id = created
            .output
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .unwrap();
        let read_wiki = registry.get("read_wiki").unwrap();
        let read = read_wiki
            .execute(&context, json!({"article_id": article_id}))
            .await
            .unwrap();
        assert_eq!(
            read.output.get("title").and_then(|value| value.as_str()),
            Some("Rust notes")
        );
        let found = read_wiki
            .execute(&context, json!({"title": "Rust"}))
            .await
            .unwrap();
        assert_eq!(
            found.output.get("id").and_then(serde_json::Value::as_i64),
            Some(article_id)
        );
        // A title lookup ignores a content-only match ("memory" is only in the
        // body, while `search_wiki` finds it).
        assert!(
            read_wiki
                .execute(&context, json!({"title": "memory"}))
                .await
                .unwrap()
                .is_error
        );
        assert!(read_wiki.execute(&context, json!({})).await.is_err());
        let search_wiki = registry.get("search_wiki").unwrap();
        let hits = search_wiki
            .execute(&context, json!({"query": "memory tools"}))
            .await
            .unwrap();
        assert_eq!(hits.output.as_array().unwrap().len(), 1);
        let update_wiki = registry.get("update_wiki").unwrap();
        let updated = update_wiki
            .execute(
                &context,
                json!({"article_id": article_id, "content": "# Notes\nupdated"}),
            )
            .await
            .unwrap();
        assert_eq!(
            updated
                .output
                .get("content")
                .and_then(|value| value.as_str()),
            Some("# Notes\nupdated")
        );

        // RSS store round-trip (the network fetch is Python-only).
        let subscribe = registry.get("rss_subscribe").unwrap();
        let subscription = subscribe
            .execute(&context, json!({"url": "https://example.com/feed.xml"}))
            .await
            .unwrap();
        assert!(
            !subscription.is_error,
            "rss_subscribe failed: {:?}",
            subscription.output
        );
        let subscription_id = subscription
            .output
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .unwrap();
        let rss_list = registry.get("rss_list").unwrap();
        let listed = rss_list.execute(&context, json!({})).await.unwrap();
        assert_eq!(listed.output.as_array().unwrap().len(), 1);
        let unsubscribe = registry.get("rss_unsubscribe").unwrap();
        let removed = unsubscribe
            .execute(&context, json!({"subscription_id": subscription_id}))
            .await
            .unwrap();
        assert!(!removed.is_error);
        let listed = rss_list.execute(&context, json!({})).await.unwrap();
        assert!(listed.output.as_array().unwrap().is_empty());

        // Task tools: natural-language schedule parsing, template expansion and
        // CRUD over the legacy store (the executor-backed `run_task_now` is out
        // of parity scope).
        let parse = registry.get("parse_cron").unwrap();
        let parsed = parse
            .execute(&context, json!({"text": "every weekday at 7:30"}))
            .await
            .unwrap();
        assert_eq!(
            parsed
                .output
                .get("cron_expression")
                .and_then(serde_json::Value::as_str),
            Some("30 7 * * 1-5")
        );
        assert_eq!(
            parsed
                .output
                .get("next_runs_utc")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(
            parse
                .execute(&context, json!({"text": "whenever the moon is full"}))
                .await
                .unwrap()
                .is_error
        );

        let create_task = registry.get("create_task").unwrap();
        let created = create_task
            .execute(
                &context,
                json!({
                    "name": "Standup digest",
                    "prompt": "Summarize the standup",
                    "schedule": "every day at 8pm",
                    "delivery_channels": ["ui"],
                }),
            )
            .await
            .unwrap();
        assert!(
            !created.is_error,
            "create_task failed: {:?}",
            created.output
        );
        let created_task = created.output.get("created").unwrap();
        assert_eq!(
            created_task
                .get("cron_expression")
                .and_then(serde_json::Value::as_str),
            Some("0 20 * * *")
        );
        let task_id = created_task
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .unwrap();
        // A template fills prompt/schedule when the caller leaves them unset.
        let templated = create_task
            .execute(&context, json!({"name": "News", "template": "news-digest"}))
            .await
            .unwrap();
        assert!(!templated.is_error, "{:?}", templated.output);
        assert_eq!(
            templated
                .output
                .get("created")
                .unwrap()
                .get("cron_expression")
                .and_then(serde_json::Value::as_str),
            Some("0 8 * * *")
        );
        // The template also supplies the prompt, workflow type, tools and the
        // tool's `max_iterations` (Python tool parity, unlike the REST path).
        let news_id = templated
            .output
            .get("created")
            .unwrap()
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .unwrap();
        let news = store.get_task("local-user", news_id).unwrap();
        assert_eq!(news.workflow_type.as_deref(), Some("news-digest"));
        assert_eq!(news.max_iterations, 12);
        assert!(!news.prompt.is_empty());
        assert!(
            !news
                .tools_whitelist
                .as_ref()
                .and_then(serde_json::Value::as_array)
                .unwrap()
                .is_empty()
        );
        // Python `or` semantics: an empty string/list counts as unset and falls
        // back to the template (not an error and not an empty allowlist).
        let empty_fallback = create_task
            .execute(
                &context,
                json!({
                    "name": "News empty",
                    "template": "news-digest",
                    "prompt": "",
                    "tools": [],
                    "delivery_channels": [],
                }),
            )
            .await
            .unwrap();
        assert!(!empty_fallback.is_error, "{:?}", empty_fallback.output);
        let empty_id = empty_fallback
            .output
            .get("created")
            .unwrap()
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .unwrap();
        let empty = store.get_task("local-user", empty_id).unwrap();
        assert!(!empty.prompt.is_empty());
        assert!(
            !empty
                .tools_whitelist
                .as_ref()
                .and_then(serde_json::Value::as_array)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            empty_fallback
                .output
                .get("created")
                .unwrap()
                .get("delivery_channels")
                .unwrap(),
            &json!(["ui"])
        );
        // An unknown template and a missing prompt/schedule fail closed.
        assert!(
            create_task
                .execute(&context, json!({"name": "x", "template": "nope"}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            create_task
                .execute(&context, json!({"name": "x"}))
                .await
                .unwrap()
                .is_error
        );

        let list_tasks = registry.get("list_tasks").unwrap();
        let listed = list_tasks.execute(&context, json!({})).await.unwrap();
        assert_eq!(listed.output.as_array().unwrap().len(), 3);

        let update_task = registry.get("update_task").unwrap();
        let updated = update_task
            .execute(&context, json!({"task_id": task_id, "enabled": false}))
            .await
            .unwrap();
        assert_eq!(
            updated
                .output
                .get("updated")
                .unwrap()
                .get("enabled")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        // A natural-language reschedule updates the cron expression.
        let rescheduled = update_task
            .execute(
                &context,
                json!({"task_id": task_id, "schedule": "every hour"}),
            )
            .await
            .unwrap();
        assert_eq!(
            rescheduled
                .output
                .get("updated")
                .unwrap()
                .get("cron_expression")
                .and_then(serde_json::Value::as_str),
            Some("0 * * * *")
        );
        // Nothing to update, and a missing task, both fail closed.
        assert!(
            update_task
                .execute(&context, json!({"task_id": task_id}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            update_task
                .execute(&context, json!({"task_id": 999_999, "name": "x"}))
                .await
                .unwrap()
                .is_error
        );

        let delete_task = registry.get("delete_task").unwrap();
        assert!(
            !delete_task
                .execute(&context, json!({"task_id": task_id}))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            delete_task
                .execute(&context, json!({"task_id": task_id}))
                .await
                .unwrap()
                .is_error
        );

        // WS2(d): the canonical catalog is a valid picker source — it contains the
        // builtins and the ported store families, with unique names.
        let catalog: Vec<String> = registry
            .catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        for expected in [
            "read_file",
            "shell",
            "memory_recall",
            "read_wiki",
            "rss_list",
            "create_task",
            "parse_cron",
        ] {
            assert!(
                catalog.iter().any(|name| name == expected),
                "catalog is missing {expected}"
            );
        }
        let mut unique = catalog.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), catalog.len(), "catalog names must be unique");
    }

    #[test]
    fn redact_location_strips_url_credentials_and_query() {
        assert_eq!(
            redact_location("https://user:token@example.com/repo?access_token=abc#frag"),
            "https://example.com/repo"
        );
        assert_eq!(redact_location("/srv/plugins/demo"), "/srv/plugins/demo");
    }

    #[test]
    fn opencode_worker_respects_explicit_overrides() {
        let fallback = std::path::Path::new("/data/plugins/opencode-data");
        let config = opencode_worker_config(
            Some(PathBuf::from("/plugins/demo/lib/index.ts")),
            Some(PathBuf::from("/opt/bun")),
            Some(PathBuf::from("/plugins/demo")),
            Some(PathBuf::from("/data/plugins/demo")),
            Some(PathBuf::from("/workspace/project")),
            fallback,
        )
        .expect("entry configured");
        assert_eq!(config.bun, PathBuf::from("/opt/bun"));
        assert_eq!(config.root, PathBuf::from("/plugins/demo"));
        assert_eq!(config.data, PathBuf::from("/data/plugins/demo"));
        assert_eq!(config.workspace, Some(PathBuf::from("/workspace/project")));
    }

    #[tokio::test]
    async fn mcp_admin_config_round_trips_and_fails_closed() {
        use cool_app_server::McpAdmin;
        let directory = tempfile::tempdir().unwrap();
        let admin = mcp_admin::CliMcpAdmin::new(directory.path());
        assert!(admin.list_servers().await.unwrap().servers.is_empty());

        let key = || {
            cool_protocol::IdempotencyKey::new(cool_app_server::client::new_idempotency_key(
                "mcp-add",
            ))
            .unwrap()
        };
        let add = |name: &str| cool_protocol::McpAddServerParams {
            idempotency_key: key(),
            name: name.to_owned(),
            transport: "stdio".to_owned(),
            command: "definitely-not-a-real-mcp-binary".to_owned(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: String::new(),
            headers: std::collections::BTreeMap::new(),
            enabled: true,
            description: String::new(),
            capabilities: Vec::new(),
            timeout_s: 5.0,
            version: String::new(),
            author: String::new(),
            compatibility: String::new(),
        };

        assert!(
            admin
                .add_server("local-user", &add("Bad Name"))
                .await
                .is_err()
        );
        // Python bounds the name at 64 characters.
        assert!(
            admin
                .add_server("local-user", &add(&"a".repeat(65)))
                .await
                .is_err()
        );
        let record = admin.add_server("local-user", &add("demo")).await.unwrap();
        assert_eq!(record.name, "demo");
        assert_eq!(record.status, "disconnected");
        // A duplicate name is rejected.
        assert!(admin.add_server("local-user", &add("demo")).await.is_err());
        assert_eq!(admin.list_servers().await.unwrap().servers.len(), 1);

        // A connect to a missing executable records an error instead of panicking.
        let result = admin.connect("local-user", "demo").await.unwrap();
        assert_eq!(result.status, "error");
        assert!(result.error.is_some());
        assert!(!admin.health("demo").await.unwrap().healthy);

        let update = cool_protocol::McpUpdateServerParams {
            idempotency_key: key(),
            name: "demo".to_owned(),
            transport: None,
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            enabled: Some(false),
            description: Some("demo server".to_owned()),
            capabilities: None,
            timeout_s: None,
        };
        let updated = admin.update_server("local-user", &update).await.unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.description, "demo server");
        // A missing server fails closed.
        let missing = cool_protocol::McpUpdateServerParams {
            name: "nope".to_owned(),
            ..update
        };
        assert!(admin.update_server("local-user", &missing).await.is_err());

        admin.remove_server("local-user", "demo").await.unwrap();
        assert!(admin.remove_server("local-user", "demo").await.is_err());
        assert!(admin.list_servers().await.unwrap().servers.is_empty());
        assert!(directory.path().join("mcp-admin-audit.jsonl").exists());
    }
}
