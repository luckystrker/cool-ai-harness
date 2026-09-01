use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, MessageRole,
    ModelDriver, OpenAiCompatibleDriver, RunOutcome, ScriptedDriver, StoreEventSink, ToolContext,
    builtin_registry,
};
use cool_app_server::{AppServer, RunLifecycle, ServerConfig, capabilities};
use cool_extensions::{
    CompatibilityAdapter, ExtensionRuntime, McpToolPolicy, PluginLoader, PluginStore,
    WorkerLaunchSpec, discover_plugin_tools_with_policy,
};
use cool_protocol::{ApprovalOutcome, CanonicalEvent};
use cool_security::{CapabilityPolicy, Decision, NetworkPolicy, Workspace};
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
        print_help();
        return Ok(());
    };
    match command.as_str() {
        "app-server" => {
            let mut transport = "stdio".to_owned();
            let mut endpoint: Option<PathBuf> = None;
            let mut data_dir = env::var_os("COOL_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data"));
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
                    _ => return Err(usage("unknown app-server argument")),
                }
            }
            match transport.as_str() {
                "stdio" if endpoint.is_none() => {}
                "local" if endpoint.is_some() => {}
                "local" => return Err(usage("local transport needs endpoint")),
                _ => return Err(usage("transport must be stdio or local")),
            }
            let store = DurableStore::open(data_dir.join("rust-core.db"))
                .map_err(|error| runtime("durable_state_failed", &error.to_string()))?;
            let config = ServerConfig::default();
            let (provider, model) = configured_provider(config.event_delay, true)?;
            let workspace = Workspace::new(
                env::current_dir()
                    .map_err(|error| runtime("workspace_failed", &error.to_string()))?,
            )
            .map_err(|error| runtime("workspace_failed", &error.to_string()))?;
            let (registry, extensions) = extension_registry(&data_dir).await;
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
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "status": "ok",
                    "phase": "M8",
                    "runtime": "rust-extension-runtime",
                    "protocolVersion": 1,
                    "capabilities": capabilities(),
                    "durableState": true,
                    "securityKernel": true,
                    "agentLoop": true,
                    "trustedTools": true,
                    "baselineProvider": "openai-compatible",
                    "plugins": true,
                    "mcp": ["stdio", "streamable-http"],
                    "hooks": true,
                    "compatibilityWorkers": ["codex", "claude"]
                }))
                .expect("doctor JSON serializes")
            );
            Ok(())
        }
        "plugin" => plugin_command(args.collect()),
        "run" => run_prompt(args.collect()).await,
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

fn plugin_command(arguments: Vec<String>) -> Result<(), (i32, serde_json::Value)> {
    if arguments.len() != 2 || arguments[0] != "doctor" {
        return Err(usage("plugin command is: plugin doctor <path>"));
    }
    let root = PathBuf::from(&arguments[1]);
    let data = root
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".plugin-data");
    let bundle = PluginLoader
        .load(&root, &data)
        .map_err(|error| runtime("plugin_load_failed", &error.to_string()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "name": bundle.manifest.as_ref().map(|value| &value.name),
            "loadable": bundle.loadable(),
            "conformant": bundle.conformant(),
            "contentHash": bundle.content_hash,
            "skills": bundle.skills,
            "mcpServers": bundle.mcp_servers,
            "hooks": bundle.hooks,
            "diagnostics": bundle.diagnostics,
        }))
        .expect("plugin doctor JSON serializes")
    );
    Ok(())
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
    let workspace = Workspace::new(
        env::current_dir().map_err(|error| runtime("workspace_failed", &error.to_string()))?,
    )
    .map_err(|error| runtime("workspace_failed", &error.to_string()))?;
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
        "Cool Rust CLI\n\nCommands:\n  app-server [--transport stdio|local] [--endpoint PATH] [--data-dir PATH]\n  serve\n  run [--scripted] <prompt>\n  plugin doctor <path>\n  doctor"
    );
}
