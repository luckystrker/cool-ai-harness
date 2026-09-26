//! M7 App Protocol server backed by the durable Rust agent runtime.
//!
//! This crate owns transport/session plumbing only. Provider, tool and policy
//! decisions remain delegated to `cool-agent`, `cool-security` and `cool-state`.

pub mod blobs;
pub mod client;
mod legacy;
mod research;
mod scheduler;
mod subagents;

pub use blobs::{BlobError, BlobStore};
pub use client::{AppClient, ClientError};
pub use research::{ResearchExecutor, ResearchOutcome};
pub use scheduler::TaskExecutor;
pub use subagents::{SubagentExecutor, SubagentLaunchSpec};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, ApprovalGate, ApprovalRequest, AutoApprovalGate,
    CancelSignal, EventSink, Message, MessageRole, RunOutcome, RuntimeError, ScriptedDriver,
    ToolContext, Usage, builtin_registry, default_agent_system_prompt, history_from_events,
    mask_canonical_event, planning_system_prompt,
};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, ApprovalResolvedResult, CanonicalEvent, Command,
    CompactResult, ContentPart, EventCursor, EventEnvelope, EventPage, ExtensionStatusResult,
    HistoryItem, HookRecord, IdempotentPlanIdParams, InitializeResult, ItemEvent, JsonRpcV2,
    LegacyOkResult, McpAddServerParams, McpConnectResult, McpHealthResult, McpServerAdminRecord,
    McpServerListResult, McpStoreInstallParams, McpStoreSearchResult, McpToolListResult,
    McpUpdateServerParams, MemoryExtractResult, ModelInfoRecord, PlanCreated, PlanExecuteResult,
    PlanProgress, PlanProgressStatus, PlanStep as ProtocolPlanStep, PluginRecord,
    PromptAcceptedResult, ProtocolError, ResponsePayload, RpcFailure, RpcId, RpcNotification,
    RpcRequest, RpcSuccess, RssFetchResult, RunCancelledResult, RunEventMethod, RunStarted,
    RunSubscribedResult, RunTerminal, ServerFrame, SessionCompacted, SessionConversationResult,
    SessionCreatedResult, SessionForkedResult, SessionHistoryResult, SessionListResult,
    SessionLoadedResult, SessionRunSummary, SessionRunsResult, SessionSummary, SkillCreateParams,
    SkillCreateResult, SkillListResult, StatusGetResult, StreamFrame, SubagentRunCancelResult,
    SystemPromptRecord, TaskRunCancelResult, TaskTemplateRecord, TextDelta, ToolCatalogRecord,
    ToolCompleted, ToolRequested, TransportLimits, UsageUpdated, V1Version,
};
use cool_security::{CapabilityPolicy, Decision, SecretKeyring, Workspace, mask_secrets};
use cool_state::{
    BudgetDelta, CancelAcceptance, ConversationLink, DurableStore, EventProvenance,
    ImportedHistoryEvent, StoreError,
};
use cool_store::LegacyStore;
use cool_store::domains::webhooks::NewWebhookEvent;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_RPC_ID_BYTES: usize = 128;
pub const RPC_METHOD: &str = "cool.command";
pub const EVENT_METHOD: &str = "run.event";

#[derive(Clone)]
pub struct ServerConfig {
    pub max_frame_bytes: usize,
    pub max_in_flight: usize,
    pub outbound_queue: usize,
    pub event_page_limit: u16,
    pub delivery_timeout: Duration,
    pub write_timeout: Duration,
    pub event_delay: Duration,
    pub request_delay: Duration,
    /// Optional legacy (`harness.db`) store serving the React surface families.
    ///
    /// `None` keeps the M9 behavior: those commands fail closed with
    /// `legacy_store_unavailable`. Opening/adopting the file is an explicit CLI
    /// decision (`cool app-server --legacy-store`); tests use an in-memory
    /// store.
    pub legacy_store: Option<Arc<LegacyStore>>,
    /// Optional Fernet keyring for provider credentials. Provider writes fail
    /// closed without it instead of persisting plaintext.
    pub secrets: Option<Arc<SecretKeyring>>,
    /// Root of the content-addressed artifact blob store (`data_dir/artifacts`
    /// on the CLI path). When unset (tests without a filesystem layout),
    /// research reports persist on the row without an artifact.
    pub artifacts_dir: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_in_flight: 16,
            outbound_queue: 64,
            event_page_limit: 256,
            delivery_timeout: Duration::from_secs(2),
            write_timeout: Duration::from_secs(2),
            event_delay: Duration::from_millis(15),
            request_delay: Duration::ZERO,
            legacy_store: None,
            secrets: None,
            artifacts_dir: None,
        }
    }
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("max_in_flight", &self.max_in_flight)
            .field("outbound_queue", &self.outbound_queue)
            .field("event_page_limit", &self.event_page_limit)
            .field("delivery_timeout", &self.delivery_timeout)
            .field("write_timeout", &self.write_timeout)
            .field("event_delay", &self.event_delay)
            .field("request_delay", &self.request_delay)
            .field(
                "legacy_store",
                &self.legacy_store.as_ref().map(|store| store.path()),
            )
            .field("secrets", &self.secrets.is_some())
            .field("artifacts_dir", &self.artifacts_dir)
            .finish()
    }
}

#[derive(Clone)]
pub struct AppServer {
    inner: Arc<Inner>,
}

struct Inner {
    config: ServerConfig,
    store: DurableStore,
    state: Mutex<State>,
    runtime: AgentRuntime,
    workspace: Workspace,
    policy: CapabilityPolicy,
    default_model: String,
    approval_waiters: Mutex<HashMap<String, watch::Sender<Option<cool_protocol::ApprovalOutcome>>>>,
    /// Connection that started each live run, so `run.subscribe` fan-out can
    /// skip it (the owner already receives events through its run sink).
    run_owners: Mutex<HashMap<String, String>>,
    /// `run.subscribe` subscribers per run, keyed by connection id.
    run_subscribers: Mutex<HashMap<String, HashMap<String, Outbound>>>,
    lifecycle: Option<Arc<dyn RunLifecycle>>,
    /// Read-only extension admin surface, present when an extension host is
    /// configured (the CLI installs one over its `PluginStore`).
    extension_admin: Option<Arc<dyn ExtensionAdmin>>,
    /// Persistent application settings, present when the host configures a
    /// settings file (the CLI stores one on the data root).
    app_settings: Option<Arc<dyn AppSettings>>,
    /// Operator-owned global MCP admin, present when the host configures it (the
    /// CLI installs one over its MCP config store + live session registry).
    mcp_admin: Option<Arc<dyn McpAdmin>>,
    /// Operator-owned global skills store, present when the host configures it.
    skill_admin: Option<Arc<dyn SkillAdmin>>,
    /// Live provider model-list probe, present when the host configures it.
    provider_probe: Option<Arc<dyn ProviderProbe>>,
    /// Forced RSS feed fetch/parse (network facade), present when the host
    /// configures it.
    rss_feed_fetch: Option<Arc<dyn RssFeedFetch>>,
    /// LLM memory extraction host (model driver + legacy store), present when
    /// the host configures it.
    memory_extractor: Option<Arc<dyn MemoryExtractor>>,
    /// Background task scheduler/executor, present when a legacy store is
    /// configured (`--legacy-store`).
    task_executor: Option<Arc<TaskExecutor>>,
    /// Foreground subagent executor, present when a legacy store is configured.
    subagent_executor: Option<Arc<SubagentExecutor>>,
    /// Deep-research pipeline executor, present when a legacy store is
    /// configured (requires the subagent executor for the gather stage).
    research_executor: Option<Arc<ResearchExecutor>>,
    /// Content-addressed artifact blob store, present when the host configured
    /// both a legacy store and an `artifacts_dir`.
    blob_store: Option<BlobStore>,
    /// Durable plan id persisted for each run's first `plan.created`, so one
    /// run's repeated `update_plan` calls do not create duplicate drafts.
    planned_runs: std::sync::Mutex<HashMap<String, i64>>,
}

#[async_trait]
pub trait RunLifecycle: Send + Sync {
    async fn on_event(
        &self,
        event: &str,
        payload: serde_json::Value,
        policy: &CapabilityPolicy,
    ) -> Vec<CanonicalEvent>;

    /// Extension status for `status.get`. `None` reports no extension runtime.
    async fn status(&self) -> Option<StatusGetResult> {
        None
    }
}

/// Read-only admin view of the installed extension host (plugins, workers, hook
/// review state, skills and plugin-bundled MCP servers) for the web admin
/// surface. The app server owns no extension state; it renders whatever the
/// host reports. Implementations must never expose secrets.
#[async_trait]
pub trait ExtensionAdmin: Send + Sync {
    /// Snapshot the installed extension state. An error is reported to the
    /// caller rather than silently degrading to an empty catalog.
    async fn status(&self) -> Result<ExtensionStatusResult, String>;

    /// Enable or disable one installed plugin. Implementations must re-verify a
    /// plugin before enabling it and return an error on a tampered tree.
    async fn set_plugin_enabled(
        &self,
        _actor: &str,
        _plugin: &str,
        _enabled: bool,
    ) -> Result<PluginRecord, String> {
        Err("extension admin does not support mutations".to_owned())
    }

    /// Approve or reject one hook declaration. Implementations must verify the
    /// submitted trust hash matches the current declaration.
    async fn set_hook_review(
        &self,
        _actor: &str,
        _plugin: &str,
        _hook: &str,
        _trust_hash: &str,
        _approved: bool,
    ) -> Result<HookRecord, String> {
        Err("extension admin does not support mutations".to_owned())
    }
}

/// Why an `mcp.store_*` registry operation failed. `store_install` reports a
/// connect failure in `McpConnectResult.error` (200-equivalent, Python parity);
/// only fetch/miss/no-packages/collision are errors here.
#[derive(Debug)]
pub enum McpStoreError {
    /// The host is not configured (`-32020`).
    Unavailable(String),
    /// Exact registry name miss (`-32004`).
    NotFound(String),
    /// The registry entry has no installable packages (`-32602`).
    NoPackages(String),
    /// A local server with the derived name already exists (`-32006`).
    AlreadyExists(String),
    /// Any other registry/host failure (`-32021`, masked detail).
    Failed(String),
}

/// Operator-owned global MCP server admin (distinct from the plugin-bundled MCP
/// servers projected by [`ExtensionAdmin`]). The app server owns no MCP state:
/// the host (the CLI) stores the operator config and manages the live session
/// registry. Records are secret-free; `env`/`headers` are accepted on writes but
/// never projected back.
#[async_trait]
pub trait McpAdmin: Send + Sync {
    /// All configured servers with their live status and discovered tools.
    async fn list_servers(&self) -> Result<McpServerListResult, String>;

    /// Add a server configuration. Implementations must reject a duplicate name
    /// and validate the transport/name.
    async fn add_server(
        &self,
        actor: &str,
        params: &McpAddServerParams,
    ) -> Result<McpServerAdminRecord, String>;

    /// Patch a server configuration. A missing server is an error.
    async fn update_server(
        &self,
        actor: &str,
        params: &McpUpdateServerParams,
    ) -> Result<McpServerAdminRecord, String>;

    /// Remove a server configuration and disconnect its live session.
    async fn remove_server(&self, actor: &str, name: &str) -> Result<(), String>;

    /// Connect to a server and discover its tools.
    async fn connect(&self, actor: &str, name: &str) -> Result<McpConnectResult, String>;

    /// Disconnect a server and drop its discovered tools.
    async fn disconnect(&self, actor: &str, name: &str) -> Result<McpConnectResult, String>;

    /// Health-check a server.
    async fn health(&self, name: &str) -> Result<McpHealthResult, String>;

    /// All tools discovered across connected servers.
    async fn list_tools(&self) -> Result<McpToolListResult, String>;

    /// Reconnect every enabled server, refreshing its tools.
    async fn reconnect_all(&self, actor: &str) -> Result<McpServerListResult, String>;

    /// Search the public MCP Registry (network; pinned to the registry host).
    async fn store_search(
        &self,
        query: &str,
        limit: u16,
    ) -> Result<McpStoreSearchResult, McpStoreError>;

    /// Popular entries from the public MCP Registry.
    async fn store_popular(&self, limit: u16) -> Result<McpStoreSearchResult, McpStoreError>;

    /// Fetch a registry entry, derive a config, then `add_server` + `connect`
    /// through this same admin (no duplicated store path).
    async fn store_install(
        &self,
        actor: &str,
        params: &McpStoreInstallParams,
    ) -> Result<McpConnectResult, McpStoreError>;
}

/// Why a `memory.extract` host call failed. Extraction itself reports skips
/// and parse/model problems in `MemoryExtractResult` (Python's 200-with-status
/// contract); these variants are host/store failures.
#[derive(Debug)]
pub enum MemoryExtractError {
    /// The host is not configured (`-32029`).
    Unavailable(String),
    /// An idempotency key conflict (`-32006`).
    Conflict(String),
    /// Any other host/store failure (`-32030`, masked detail).
    Failed(String),
}

/// LLM-backed memory extraction over one conversation (Workstream B4c). The
/// host owns the model driver and the legacy memory store; the app server
/// only forwards. Extraction is a one-shot completion plus a best-effort
/// conflict-detection completion — never a durable agent run.
#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    async fn extract(
        &self,
        actor: &str,
        conversation_id: i64,
        idempotency_key: &str,
    ) -> Result<MemoryExtractResult, MemoryExtractError>;
}

/// Persistent application settings (the default system prompt today). The host
/// (the CLI) owns the storage; the app server only forwards and renders.
#[async_trait]
pub trait AppSettings: Send + Sync {
    /// The persisted default system prompt record (may be empty).
    async fn system_prompt(&self) -> Result<SystemPromptRecord, String>;
    /// Replace the persisted default system prompt.
    async fn set_system_prompt(&self, prompt: &str) -> Result<SystemPromptRecord, String>;
}

/// Operator-owned global skills store (distinct from the plugin-bundled skills
/// projected by [`ExtensionAdmin`]). The host (the CLI) owns the `SKILL.md`
/// directory tree; the app server forwards and renders. A skill body is
/// instructions, never executed here.
#[async_trait]
pub trait SkillAdmin: Send + Sync {
    /// List skills, optionally filtered by source.
    async fn list(&self, source: Option<&str>) -> Result<SkillListResult, String>;

    /// Create a skill. Implementations must validate the name and reject a
    /// duplicate.
    async fn create(
        &self,
        actor: &str,
        params: &SkillCreateParams,
    ) -> Result<SkillCreateResult, String>;

    /// Delete a skill by name. A missing skill is an error.
    async fn delete(&self, actor: &str, name: &str) -> Result<(), String>;
}

/// Live provider model-list probe. The host (the CLI) owns the provider
/// credentials and egress policy; the app server forwards. A probe error is
/// reported to the caller rather than degrading to the cached catalog.
#[async_trait]
pub trait ProviderProbe: Send + Sync {
    /// Live model list for an already-saved provider row (decrypts the stored key).
    async fn list_models(
        &self,
        actor: &str,
        provider_id: i64,
    ) -> Result<Vec<ModelInfoRecord>, String>;

    /// Live model-list probe for an unsaved provider. The plaintext `api_key` is
    /// used in memory only and never persisted.
    async fn preview_models(
        &self,
        name: &str,
        base_url: Option<&str>,
        api_key: &str,
    ) -> Result<Vec<ModelInfoRecord>, String>;
}

/// Why an `rss.fetch_now` failed. Fetch/parse problems are **not** errors: they
/// record `last_error` on the subscription and answer `new_entries: 0`.
#[derive(Debug)]
pub enum RssFetchError {
    /// The subscription id is unknown to the actor (`-32004`).
    NotFound,
    /// Idempotency key conflict (`-32006`).
    Conflict(String),
    /// The host or legacy store is not configured (`-32027`).
    Unavailable(String),
    /// Any other host failure (`-32028`, masked detail).
    Failed(String),
}

/// Forced RSS feed fetch/parse (the network facade). The host owns the egress
/// policy and the legacy store writes; a fetch/parse failure is recorded on the
/// subscription and reported as zero new entries (Python `fetch_feed` parity),
/// while a missing subscription or a store/host failure is an error.
#[async_trait]
pub trait RssFeedFetch: Send + Sync {
    /// Fetch the subscription's feed now (ignores the fetch interval).
    ///
    /// The mutation is idempotent on `(actor, idempotency_key)`.
    async fn fetch_now(
        &self,
        actor: &str,
        subscription_id: i64,
        idempotency_key: &str,
    ) -> Result<RssFetchResult, RssFetchError>;
}

#[derive(Default)]
struct State {
    runs: HashMap<String, RunRecord>,
    prompt_executions: u64,
}

struct RunRecord {
    cancel: watch::Sender<Option<String>>,
    terminal: bool,
}

/// Canonical inputs for one prompt run, decoupled from the protocol params so
/// the spawned run does not retain the whole request envelope.
struct PromptRequest {
    content: String,
    model: Option<String>,
    system_prompt: Option<String>,
    plan_mode: bool,
}

struct ConnectionState {
    initialized: bool,
    /// Stable per-connection id, so live run events can fan out to
    /// `run.subscribe` connections while the owner still receives them.
    id: String,
    owned_runs: HashSet<String>,
    subscribed_runs: HashSet<String>,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            initialized: false,
            id: format!("connection-{}", Uuid::new_v4()),
            owned_runs: HashSet::new(),
            subscribed_runs: HashSet::new(),
        }
    }
}

#[derive(Clone)]
struct Outbound {
    sender: mpsc::Sender<ServerFrame>,
    failed: watch::Sender<bool>,
    deadline: Duration,
}

impl Outbound {
    async fn send(&self, frame: ServerFrame) -> bool {
        let delivered = timeout(self.deadline, self.sender.send(frame))
            .await
            .is_ok_and(|result| result.is_ok());
        if !delivered {
            let _ = self.failed.send(true);
        }
        delivered
    }
}

impl AppServer {
    pub fn new(config: ServerConfig) -> Self {
        Self::build(
            config,
            DurableStore::in_memory().expect("in-memory Rust store must initialize"),
        )
    }

    pub fn with_store(config: ServerConfig, store: DurableStore) -> Result<Self, StoreError> {
        store.recover_incomplete_runs()?;
        Ok(Self::build(config, store))
    }

    fn build(config: ServerConfig, store: DurableStore) -> Self {
        let workspace = std::env::current_dir()
            .ok()
            .and_then(|path| Workspace::new(path).ok())
            .expect("current directory must be a valid workspace");
        let runtime = AgentRuntime::new(
            Arc::new(ScriptedDriver::echo_with_delay(config.event_delay)),
            builtin_registry(),
        );
        Self::build_with_runtime(
            config,
            store,
            runtime,
            workspace,
            CapabilityPolicy::new(Some(Decision::Ask)),
            "scripted-echo".to_owned(),
        )
    }

    pub fn with_agent_runtime(
        config: ServerConfig,
        store: DurableStore,
        runtime: AgentRuntime,
        workspace: Workspace,
        policy: CapabilityPolicy,
        default_model: impl Into<String>,
    ) -> Result<Self, StoreError> {
        store.recover_incomplete_runs()?;
        Ok(Self::build_with_runtime(
            config,
            store,
            runtime,
            workspace,
            policy,
            default_model.into(),
        ))
    }

    fn build_with_runtime(
        config: ServerConfig,
        store: DurableStore,
        runtime: AgentRuntime,
        workspace: Workspace,
        policy: CapabilityPolicy,
        default_model: String,
    ) -> Self {
        assert!(config.max_in_flight > 0, "max_in_flight must be positive");
        assert!(
            config.max_in_flight <= u16::MAX as usize,
            "max_in_flight exceeds the protocol limit type"
        );
        assert!(config.outbound_queue > 0, "outbound_queue must be positive");
        assert!(
            config.outbound_queue <= u16::MAX as usize,
            "outbound_queue exceeds the protocol limit type"
        );
        assert!(
            config.max_frame_bytes <= u32::MAX as usize,
            "max_frame_bytes exceeds the protocol limit type"
        );
        assert!(
            config.max_frame_bytes >= 256,
            "max_frame_bytes is too small for structured errors"
        );
        assert!(
            config.event_page_limit > 0,
            "event_page_limit must be positive"
        );
        assert!(
            !config.delivery_timeout.is_zero(),
            "delivery_timeout must be positive"
        );
        assert!(
            !config.write_timeout.is_zero(),
            "write_timeout must be positive"
        );
        let task_executor = config.legacy_store.as_ref().map(|legacy| {
            Arc::new(TaskExecutor::new(
                Arc::clone(legacy),
                runtime.clone(),
                workspace.clone(),
                policy.clone(),
                default_model.clone(),
                cool_store::scheduler::SchedulerConfig::default(),
            ))
        });
        let subagent_executor = config.legacy_store.as_ref().map(|legacy| {
            Arc::new(SubagentExecutor::new(
                Arc::clone(legacy),
                store.clone(),
                runtime.clone(),
                workspace.clone(),
                policy.clone(),
                default_model.clone(),
            ))
        });
        let research_executor = config
            .legacy_store
            .as_ref()
            .zip(subagent_executor.as_ref())
            .map(|(legacy, subagents)| {
                Arc::new(ResearchExecutor::new(
                    Arc::clone(legacy),
                    Arc::clone(subagents),
                    &runtime,
                    default_model.clone(),
                    config.artifacts_dir.clone(),
                ))
            });
        let blob_store = config
            .legacy_store
            .as_ref()
            .zip(config.artifacts_dir.as_ref())
            .map(|(legacy, root)| BlobStore::new(Arc::clone(legacy), root.clone()));
        Self {
            inner: Arc::new(Inner {
                config,
                store,
                state: Mutex::new(State::default()),
                runtime,
                workspace,
                policy,
                default_model,
                approval_waiters: Mutex::new(HashMap::new()),
                run_owners: Mutex::new(HashMap::new()),
                run_subscribers: Mutex::new(HashMap::new()),
                lifecycle: None,
                extension_admin: None,
                app_settings: None,
                mcp_admin: None,
                skill_admin: None,
                provider_probe: None,
                rss_feed_fetch: None,
                memory_extractor: None,
                task_executor,
                subagent_executor,
                research_executor,
                blob_store,
                planned_runs: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The background task executor, if a legacy store is configured. Callers
    /// (the CLI) start its loop with [`TaskExecutor::spawn_loop`].
    pub fn task_executor(&self) -> Option<Arc<TaskExecutor>> {
        self.inner.task_executor.clone()
    }

    /// The foreground subagent executor (for agent tools that delegate).
    pub fn subagent_executor(&self) -> Option<Arc<SubagentExecutor>> {
        self.inner.subagent_executor.clone()
    }

    /// The deep-research pipeline executor (for the `deep_research` tool and
    /// the research dispatch kickoff).
    pub fn research_executor(&self) -> Option<Arc<ResearchExecutor>> {
        self.inner.research_executor.clone()
    }

    /// The operator-owned skills admin, for skill tools bound after build.
    pub fn skill_admin(&self) -> Option<Arc<dyn SkillAdmin>> {
        self.inner.skill_admin.clone()
    }

    /// The content-addressed blob store, when both a legacy store and an
    /// artifacts directory are configured (the CLI sets both).
    pub fn blob_store(&self) -> Option<BlobStore> {
        self.inner.blob_store.clone()
    }

    pub fn with_run_lifecycle(mut self, lifecycle: Arc<dyn RunLifecycle>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("run lifecycle must be configured before the server is cloned")
            .lifecycle = Some(lifecycle);
        self
    }

    pub fn with_extension_admin(mut self, admin: Arc<dyn ExtensionAdmin>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("extension admin must be configured before the server is cloned")
            .extension_admin = Some(admin);
        self
    }

    pub fn with_app_settings(mut self, settings: Arc<dyn AppSettings>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("app settings must be configured before the server is cloned")
            .app_settings = Some(settings);
        self
    }

    pub fn with_mcp_admin(mut self, admin: Arc<dyn McpAdmin>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("mcp admin must be configured before the server is cloned")
            .mcp_admin = Some(admin);
        self
    }

    pub fn with_skill_admin(mut self, admin: Arc<dyn SkillAdmin>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("skill admin must be configured before the server is cloned")
            .skill_admin = Some(admin);
        self
    }

    pub fn with_provider_probe(mut self, probe: Arc<dyn ProviderProbe>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("provider probe must be configured before the server is cloned")
            .provider_probe = Some(probe);
        self
    }

    pub fn with_rss_feed_fetch(mut self, fetch: Arc<dyn RssFeedFetch>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("rss feed fetch must be configured before the server is cloned")
            .rss_feed_fetch = Some(fetch);
        self
    }

    pub fn with_memory_extractor(mut self, extractor: Arc<dyn MemoryExtractor>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("memory extractor must be configured before the server is cloned")
            .memory_extractor = Some(extractor);
        self
    }

    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    pub async fn serve_io<T>(&self, io: T) -> io::Result<()>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, mut writer) = tokio::io::split(io);
        let mut reader = BufReader::new(reader);
        let (outbound_sender, mut outbound_rx) = mpsc::channel(self.inner.config.outbound_queue);
        let max_frame_bytes = self.inner.config.max_frame_bytes;
        let delivery_timeout = self.inner.config.delivery_timeout;
        let write_timeout = self.inner.config.write_timeout;
        let (connection_failed, mut connection_failed_rx) = watch::channel(false);
        let outbound = Outbound {
            sender: outbound_sender,
            failed: connection_failed.clone(),
            deadline: delivery_timeout,
        };
        let writer_task = tokio::spawn(async move {
            let result = async {
                while let Some(frame) = outbound_rx.recv().await {
                    let encoded = encode_bounded_frame(frame, max_frame_bytes)?;
                    timeout(write_timeout, async {
                        writer.write_all(&encoded).await?;
                        writer.write_all(b"\n").await?;
                        writer.flush().await
                    })
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "frame delivery timed out")
                    })??;
                }
                Ok::<(), io::Error>(())
            }
            .await;
            if result.is_err() {
                let _ = connection_failed.send(true);
            }
            result
        });

        let connection = Arc::new(Mutex::new(ConnectionState::new()));
        let semaphore = Arc::new(Semaphore::new(self.inner.config.max_in_flight));
        let mut handlers = JoinSet::new();
        let mut read_error = None;

        loop {
            while handlers.try_join_next().is_some() {}
            let read = tokio::select! {
                result = read_bounded_line(&mut reader, self.inner.config.max_frame_bytes) => Some(result),
                changed = connection_failed_rx.changed() => {
                    if changed.is_ok() && *connection_failed_rx.borrow() {
                        None
                    } else {
                        continue;
                    }
                }
            };
            let Some(read) = read else {
                break;
            };
            let line = match read {
                Ok(line) => line,
                Err(error) => {
                    read_error = Some(error);
                    break;
                }
            };
            match line {
                BoundedLine::Eof => break,
                BoundedLine::TooLarge => {
                    if !outbound
                        .send(failure(
                            RpcId::Null,
                            error(-32700, "frame_too_large", false),
                        ))
                        .await
                    {
                        break;
                    }
                }
                BoundedLine::Line(line) => {
                    let value = match serde_json::from_slice::<serde_json::Value>(&line) {
                        Ok(value) => value,
                        Err(_) => {
                            if !outbound
                                .send(failure(RpcId::Null, error(-32700, "parse_error", false)))
                                .await
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    let error_id = rpc_id_from_value(&value);
                    let invalid_code = classify_invalid_request(&value);
                    let request = match serde_json::from_value::<RpcRequest>(value) {
                        Ok(request) => request,
                        Err(_) => {
                            let cool_code = match invalid_code {
                                -32601 => "method_not_found",
                                -32602 => "invalid_params",
                                _ => "invalid_request",
                            };
                            if !outbound
                                .send(failure(error_id, error(invalid_code, cool_code, false)))
                                .await
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    if !rpc_id_within_limit(&request.id) {
                        if !outbound
                            .send(failure(
                                RpcId::Null,
                                error(-32600, "rpc_id_too_large", false),
                            ))
                            .await
                        {
                            break;
                        }
                        continue;
                    }
                    if !connection.lock().await.initialized {
                        if matches!(&request.params.command, Command::Initialize(_)) {
                            // The dispatch future is very large (one arm per
                            // command) and is constructed when the wrapper
                            // future is first polled. Spawn that wrapper so the
                            // large state machine is built on a worker stack,
                            // not the serve loop's stack, then wait so
                            // initialization still completes before the next
                            // request is read.
                            let server = self.clone();
                            let outbound = outbound.clone();
                            let connection = connection.clone();
                            let (done, finished) = tokio::sync::oneshot::channel();
                            handlers.spawn(async move {
                                server.dispatch(request, outbound, connection).await;
                                let _ = done.send(());
                            });
                            let _ = finished.await;
                        } else if !outbound
                            .send(failure(request.id, error(-32002, "not_initialized", false)))
                            .await
                        {
                            break;
                        }
                        continue;
                    }
                    let permit = match semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            if !outbound
                                .send(failure(
                                    request.id,
                                    error(-32001, "server_overloaded", true),
                                ))
                                .await
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    let server = self.clone();
                    let outbound = outbound.clone();
                    let connection = connection.clone();
                    handlers.spawn(async move {
                        let _permit = permit;
                        // Same wrapper shape as the initialize path: keep the
                        // huge dispatch state machine off the spawn site.
                        server.dispatch(request, outbound, connection).await;
                    });
                }
            }
        }

        while handlers.join_next().await.is_some() {}
        let (connection_id, owned_runs, subscribed_runs) = {
            let connection = connection.lock().await;
            (
                connection.id.clone(),
                connection.owned_runs.clone(),
                connection.subscribed_runs.clone(),
            )
        };
        for run_id in owned_runs {
            self.signal_cancel(&run_id, "disconnect").await;
        }
        self.drop_connection_registrations(&connection_id, &subscribed_runs)
            .await;
        drop(outbound);
        if *connection_failed_rx.borrow() && !writer_task.is_finished() {
            writer_task.abort();
            let _ = writer_task.await;
            if let Some(error) = read_error {
                return Err(error);
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "outbound delivery failed",
            ));
        }
        let writer_result = match writer_task.await.map_err(io::Error::other)? {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        };
        if let Some(error) = read_error {
            return Err(error);
        }
        writer_result
    }

    pub async fn serve_stdio(&self) -> io::Result<()> {
        let io = StdioIo {
            reader: tokio::io::stdin(),
            writer: tokio::io::stdout(),
        };
        self.serve_io(io).await
    }

    #[cfg(unix)]
    pub async fn serve_local(&self, endpoint: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        use tokio::net::UnixListener;

        let listener = UnixListener::bind(endpoint)?;
        if let Err(error) =
            std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))
        {
            drop(listener);
            let _ = std::fs::remove_file(endpoint);
            return Err(error);
        }
        let _cleanup = SocketCleanup(endpoint.to_owned());
        loop {
            let (stream, _) = listener.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                let _ = server.serve_io(stream).await;
            });
        }
    }

    #[cfg(windows)]
    pub async fn serve_local(&self, endpoint: &Path) -> io::Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let name = endpoint.to_string_lossy().into_owned();
        let mut first = true;
        loop {
            let pipe = ServerOptions::new()
                .first_pipe_instance(first)
                .create(&name)?;
            first = false;
            pipe.connect().await?;
            let server = self.clone();
            tokio::spawn(async move {
                let _ = server.serve_io(pipe).await;
            });
        }
    }

    pub async fn events_for_run(&self, run_id: &str) -> Option<Vec<EventEnvelope>> {
        self.inner.store.all_events(run_id, &local_actor().id).ok()
    }

    pub fn store(&self) -> &DurableStore {
        &self.inner.store
    }

    pub fn create_approval(
        &self,
        session_id: &str,
        run_id: &str,
        call_id: &str,
        tool_name: &str,
        reason: &str,
    ) -> Result<cool_state::ApprovalTicket, StoreError> {
        self.inner.store.create_approval(
            &local_actor().id,
            session_id,
            run_id,
            call_id,
            tool_name,
            reason,
        )
    }

    pub async fn prompt_executions(&self) -> u64 {
        self.inner.state.lock().await.prompt_executions
    }

    async fn dispatch(
        &self,
        request: RpcRequest,
        outbound: Outbound,
        connection: Arc<Mutex<ConnectionState>>,
    ) {
        if !self.inner.config.request_delay.is_zero() {
            sleep(self.inner.config.request_delay).await;
        }
        let id = request.id.clone();

        match request.params.command {
            Command::Initialize(params) => {
                if !params.supported_protocol_versions.contains(&1) {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32003, "protocol_version_unsupported", false)),
                        )
                        .await;
                    return;
                }
                let mut connection = connection.lock().await;
                if connection.initialized {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32600, "already_initialized", false)),
                        )
                        .await;
                    return;
                }
                connection.initialized = true;
                drop(connection);
                let result = InitializeResult {
                    protocol_version: V1Version::VALUE,
                    server_name: "cool-app-server".to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    capabilities: capabilities(),
                    limits: self.transport_limits(),
                };
                let _ = self
                    .send(&outbound, success(id, ResponsePayload::Initialized(result)))
                    .await;
            }
            Command::SessionCreate(params) => {
                if let Some(error) = validate_label(
                    "title",
                    params.title.as_deref(),
                    "project_key",
                    params.project_key.as_deref(),
                ) {
                    let _ = self.send(&outbound, failure(id, error)).await;
                    return;
                }
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                let result = self
                    .create_session(
                        &actor.id,
                        params.idempotency_key.as_str(),
                        fingerprint,
                        params.title,
                        params.project_key,
                    )
                    .await;
                let frame = match result {
                    Ok(session_id) => success(
                        id,
                        ResponsePayload::SessionCreated(SessionCreatedResult { session_id }),
                    ),
                    Err(error) => failure(id, error),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionLoad(params) => {
                let result = self.load_session(&params.session_id).await;
                let frame = match result {
                    Some(result) => success(id, ResponsePayload::SessionLoaded(result)),
                    None => failure(id, error(-32004, "session_not_found", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionList(params) => {
                let frame = match self.session_list(params.project_key.as_deref(), params.limit) {
                    Ok(result) => success(id, ResponsePayload::SessionListed(result)),
                    Err(error) => failure(id, error),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionHistory(params) => {
                let frame = match self.session_history(
                    &id,
                    &params.session_id,
                    params.limit,
                    params.before_cursor,
                ) {
                    Ok(result) => success(id, ResponsePayload::SessionHistory(result)),
                    Err(error) => failure(id, error),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionFork(params) => {
                if let Some(error) = validate_label("title", params.title.as_deref(), "title", None)
                {
                    let _ = self.send(&outbound, failure(id, error)).await;
                    return;
                }
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                let frame = match self.inner.store.fork_session(
                    &actor.id,
                    params.idempotency_key.as_str(),
                    &fingerprint,
                    &params.session_id,
                    params.title.as_deref(),
                ) {
                    Ok(forked) => success(
                        id,
                        ResponsePayload::SessionForked(SessionForkedResult {
                            session_id: forked.value,
                            forked_from: params.session_id,
                        }),
                    ),
                    Err(store) => failure(id, store_error(store)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionForConversation(params) => {
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                // Replays answer from the durable idempotency record before
                // touching the legacy store, so repeating the command is cheap
                // and still returns the original outcome after the source
                // conversation was deleted.
                match self.inner.store.lookup_idempotent::<ConversationLink>(
                    &actor.id,
                    "session.for_conversation",
                    params.idempotency_key.as_str(),
                    &fingerprint,
                ) {
                    Ok(Some(link)) => {
                        let _ = self
                            .send(&outbound, success(id, session_conversation_payload(link)))
                            .await;
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = self.send(&outbound, failure(id, store_error(error))).await;
                        return;
                    }
                }
                let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32010, "legacy_store_unavailable", false)),
                        )
                        .await;
                    return;
                };
                let conversation = match legacy.get_conversation(&actor.id, params.conversation_id)
                {
                    Ok(conversation) => conversation,
                    Err(error) => {
                        let _ = self
                            .send(&outbound, failure(id, legacy::store_error(error)))
                            .await;
                        return;
                    }
                };
                let window = match legacy.recent_messages(
                    &actor.id,
                    params.conversation_id,
                    MAX_IMPORTED_MESSAGES,
                ) {
                    Ok(window) => window,
                    Err(error) => {
                        let _ = self
                            .send(&outbound, failure(id, legacy::store_error(error)))
                            .await;
                        return;
                    }
                };
                let mut messages = window.messages;
                let trimmed = trim_orphan_tool_rows(&mut messages);
                // `truncated` means the projection is not the complete legacy
                // transcript: an older window exists and/or leading orphan
                // tool rows were dropped.
                let truncated = window.has_more || trimmed > 0;
                let history = legacy_history_events(&messages);
                let frame = match self.inner.store.link_conversation(
                    &actor.id,
                    params.idempotency_key.as_str(),
                    &fingerprint,
                    params.conversation_id,
                    conversation.title.as_deref(),
                    conversation.working_directory.as_deref(),
                    &history,
                    truncated,
                ) {
                    Ok(link) => success(id, session_conversation_payload(link)),
                    Err(error) => failure(id, store_error(error)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionRuns(params) => {
                if params.limit == 0 || params.limit > self.inner.config.event_page_limit {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32602, "invalid_session_runs_limit", false)),
                        )
                        .await;
                    return;
                }
                let actor = local_actor();
                let frame = match self.inner.store.list_session_runs(
                    &actor.id,
                    &params.session_id,
                    usize::from(params.limit),
                ) {
                    Ok(runs) => success(
                        id,
                        ResponsePayload::SessionRuns(SessionRunsResult {
                            runs: runs
                                .into_iter()
                                .map(|run| SessionRunSummary {
                                    run_id: run.run_id,
                                    status: run.status.as_str().to_owned(),
                                    last_seq: run.last_seq,
                                    finish_reason: run.finish_reason,
                                    updated_at: run.updated_at,
                                })
                                .collect(),
                        }),
                    ),
                    Err(error) => failure(id, store_error(error)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionSteer(params) => {
                if params
                    .content
                    .iter()
                    .any(|part| !matches!(part, ContentPart::Text { .. }))
                {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32602, "unsupported_content_part", false)),
                        )
                        .await;
                    return;
                }
                let content = params
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if content.trim().is_empty() {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32602, "empty_steer_content", false)),
                        )
                        .await;
                    return;
                }
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                let frame = match self.inner.store.steer_run(
                    &actor.id,
                    params.idempotency_key.as_str(),
                    &fingerprint,
                    &params.run_id,
                    &mask_secrets(&content),
                ) {
                    Ok(steer) => {
                        let frame = success(id, ResponsePayload::SteerAccepted(steer.value));
                        if steer.created
                            && let Ok(events) =
                                self.inner
                                    .store
                                    .events(&params.run_id, &actor.id, None, usize::MAX)
                            && let Some(event) = events.into_iter().last()
                        {
                            self.publish_to_subscribers(&event).await;
                            let _ = self.send(&outbound, notification(event)).await;
                        }
                        frame
                    }
                    Err(store) => failure(id, store_error(store)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SessionPrompt(params) => {
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                match self
                    .existing_prompt(&actor.id, params.idempotency_key.as_str(), &fingerprint)
                    .await
                {
                    Ok(Some(run_id)) => {
                        let _ = self
                            .send(
                                &outbound,
                                success(
                                    id,
                                    ResponsePayload::PromptAccepted(PromptAcceptedResult {
                                        run_id,
                                    }),
                                ),
                            )
                            .await;
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = self.send(&outbound, failure(id, error)).await;
                        return;
                    }
                }
                let content =
                    match self.expand_prompt_parts(&actor, &params.session_id, &params.content) {
                        Ok(content) => content,
                        Err(error) => {
                            let _ = self.send(&outbound, failure(id, error)).await;
                            return;
                        }
                    };
                if !self.prompt_start_frames_fit(
                    &params.session_id,
                    &content,
                    params.model.as_deref(),
                ) {
                    let _ = self
                        .send(
                            &outbound,
                            failure(id, error(-32008, "outbound_frame_too_large", false)),
                        )
                        .await;
                    return;
                }
                match self
                    .start_prompt(
                        &actor.id,
                        params.idempotency_key.as_str(),
                        fingerprint,
                        &params.session_id,
                    )
                    .await
                {
                    Ok((run_id, cancel, is_new)) => {
                        let _ = self
                            .send(
                                &outbound,
                                success(
                                    id,
                                    ResponsePayload::PromptAccepted(PromptAcceptedResult {
                                        run_id: run_id.clone(),
                                    }),
                                ),
                            )
                            .await;
                        if is_new {
                            let connection_id = connection.lock().await.id.clone();
                            connection.lock().await.owned_runs.insert(run_id.clone());
                            self.register_run_owner(&run_id, &connection_id).await;
                            self.spawn_agent_run(
                                run_id,
                                PromptRequest {
                                    content,
                                    model: params.model,
                                    system_prompt: params.system_prompt,
                                    plan_mode: params.plan_mode,
                                },
                                cancel,
                                outbound,
                            );
                        }
                    }
                    Err(error) => {
                        let _ = self.send(&outbound, failure(id, error)).await;
                    }
                }
            }
            Command::RunCancel(params) => {
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                let reason = params.reason.as_deref().unwrap_or("client");
                let existing = self.inner.store.lookup_idempotent::<RunCancelledResult>(
                    &actor.id,
                    "run.cancel",
                    params.idempotency_key.as_str(),
                    &fingerprint,
                );
                match existing {
                    Err(store) => {
                        let _ = self.send(&outbound, failure(id, store_error(store))).await;
                        return;
                    }
                    Ok(Some(_)) => {}
                    Ok(None)
                        if !self
                            .run_event_frame_fits(
                                &params.run_id,
                                CanonicalEvent::RunCancelled(RunTerminal {
                                    reason: reason.to_owned(),
                                    error_code: None,
                                }),
                            )
                            .await =>
                    {
                        let _ = self
                            .send(
                                &outbound,
                                failure(id, error(-32008, "outbound_frame_too_large", false)),
                            )
                            .await;
                        return;
                    }
                    Ok(None) => {}
                }
                let frame = match self
                    .cancel_run(
                        &actor.id,
                        params.idempotency_key.as_str(),
                        fingerprint,
                        &params.run_id,
                        reason,
                    )
                    .await
                {
                    Ok(acceptance) => {
                        let frame =
                            success(id, ResponsePayload::RunCancelled(acceptance.result.clone()));
                        // Subscribers get every event even if the owner's
                        // connection has already failed.
                        for event in &acceptance.events {
                            self.publish_to_subscribers(event).await;
                        }
                        if self.send(&outbound, frame).await {
                            for event in acceptance.events {
                                if !self.send(&outbound, notification(event)).await {
                                    break;
                                }
                            }
                        }
                        return;
                    }
                    Err(error) => failure(id, error),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::RunEvents(params) => {
                let page = self
                    .event_page(&id, &params.run_id, params.after_seq, params.limit)
                    .await;
                let frame = match page {
                    Ok(page) => success(id, ResponsePayload::EventPage(page)),
                    Err(error) => failure(id, error),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::RunSubscribe(params) => {
                let actor = local_actor();
                let run = match self.inner.store.run(&params.run_id, &actor.id) {
                    Ok(run) => run,
                    Err(error) => {
                        let _ = self.send(&outbound, failure(id, store_error(error))).await;
                        return;
                    }
                };
                let connection_id = connection.lock().await.id.clone();
                let mut terminal = run.status.is_terminal();
                let mut last_seq = run.last_seq;
                if !terminal {
                    self.register_subscriber(&params.run_id, &connection_id, &outbound)
                        .await;
                    connection
                        .lock()
                        .await
                        .subscribed_runs
                        .insert(params.run_id.clone());
                    // Re-read after registering: the run may have terminated in
                    // between, in which case its terminal fan-out already ran
                    // and removed the subscriber map, so this connection would
                    // otherwise wait for an event that never comes.
                    if let Ok(run) = self.inner.store.run(&params.run_id, &actor.id) {
                        terminal = run.status.is_terminal();
                        last_seq = run.last_seq;
                    }
                    if terminal {
                        self.unregister_subscriber(&params.run_id, &connection_id)
                            .await;
                        connection
                            .lock()
                            .await
                            .subscribed_runs
                            .remove(&params.run_id);
                    }
                }
                let _ = self
                    .send(
                        &outbound,
                        success(
                            id,
                            ResponsePayload::RunSubscribed(RunSubscribedResult {
                                run_id: params.run_id,
                                session_id: run.session_id,
                                last_seq,
                                terminal,
                            }),
                        ),
                    )
                    .await;
            }
            Command::ApprovalResolve(params) => {
                let actor = local_actor();
                let fingerprint = fingerprint(&params);
                let resolved = self.inner.store.resolve_approval(
                    &actor.id,
                    params.idempotency_key.as_str(),
                    &fingerprint,
                    &params.approval_id,
                    params.expected_revision,
                    params.decision,
                );
                match resolved {
                    Ok(resolution) => {
                        if let Some(waiter) = self
                            .inner
                            .approval_waiters
                            .lock()
                            .await
                            .remove(&resolution.approval_id)
                        {
                            let _ = waiter.send(Some(resolution.outcome.clone()));
                        }
                        let response = ApprovalResolvedResult {
                            approval_id: resolution.approval_id,
                            revision: resolution.revision,
                            outcome: resolution.outcome,
                        };
                        if self
                            .send(
                                &outbound,
                                success(id, ResponsePayload::ApprovalResolved(response)),
                            )
                            .await
                            && resolution.created
                        {
                            let event = resolution.event;
                            self.publish_to_subscribers(&event).await;
                            let _ = self.send(&outbound, notification(event)).await;
                        }
                    }
                    Err(store) => {
                        let _ = self.send(&outbound, failure(id, store_error(store))).await;
                    }
                }
            }
            Command::StatusGet(_) => {
                let status = match &self.inner.lifecycle {
                    Some(lifecycle) => lifecycle.status().await.unwrap_or_default(),
                    None => StatusGetResult::default(),
                };
                let _ = self
                    .send(&outbound, success(id, ResponsePayload::Status(status)))
                    .await;
            }
            Command::ToolsList(_) => {
                let _ = self
                    .send(
                        &outbound,
                        success(id, ResponsePayload::ToolsListed(self.tool_catalog())),
                    )
                    .await;
            }
            Command::ExtensionsStatus(_) => {
                let frame = match &self.inner.extension_admin {
                    Some(admin) => match admin.status().await {
                        Ok(status) => success(id, ResponsePayload::ExtensionsStatus(status)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32015, "extension_state_failed", &message),
                        ),
                    },
                    None => success(
                        id,
                        ResponsePayload::ExtensionsStatus(ExtensionStatusResult::default()),
                    ),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::ExtensionsPluginEnabled(params) => {
                let frame = match &self.inner.extension_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin
                            .set_plugin_enabled(&actor, &params.plugin, params.enabled)
                            .await
                        {
                            Ok(record) => {
                                success(id, ResponsePayload::ExtensionsPluginEnabled(record))
                            }
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32017, "extension_mutation_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32016, "extension_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::ExtensionsHookReview(params) => {
                let frame = match &self.inner.extension_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin
                            .set_hook_review(
                                &actor,
                                &params.plugin,
                                &params.hook,
                                &params.trust_hash,
                                params.approved,
                            )
                            .await
                        {
                            Ok(record) => {
                                success(id, ResponsePayload::ExtensionsHookReviewed(record))
                            }
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32017, "extension_mutation_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32016, "extension_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SettingsSystemPrompt(_) => {
                let frame = match &self.inner.app_settings {
                    Some(settings) => match settings.system_prompt().await {
                        Ok(record) => success(id, ResponsePayload::SettingsSystemPrompt(record)),
                        Err(message) => {
                            failure(id, masked_detail_error(-32018, "settings_failed", &message))
                        }
                    },
                    // No settings file configured: report the built-in default
                    // (empty, so runs send no system message) rather than fail.
                    None => success(
                        id,
                        ResponsePayload::SettingsSystemPrompt(SystemPromptRecord {
                            prompt: String::new(),
                            is_custom: false,
                            source: "builtin".to_owned(),
                        }),
                    ),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SettingsSystemPromptSet(params) => {
                let frame = match &self.inner.app_settings {
                    Some(settings) => match settings.set_system_prompt(&params.prompt).await {
                        Ok(record) => success(id, ResponsePayload::SettingsSystemPrompt(record)),
                        Err(message) => {
                            failure(id, masked_detail_error(-32018, "settings_failed", &message))
                        }
                    },
                    None => failure(id, error(-32019, "settings_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpListServers(_) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => match admin.list_servers().await {
                        Ok(result) => success(id, ResponsePayload::McpServersListed(result)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32021, "mcp_admin_failed", &message),
                        ),
                    },
                    None => success(
                        id,
                        ResponsePayload::McpServersListed(McpServerListResult::default()),
                    ),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpAddServer(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.add_server(&actor, &params).await {
                            Ok(record) => success(id, ResponsePayload::McpServerAdded(record)),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32021, "mcp_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpUpdateServer(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.update_server(&actor, &params).await {
                            Ok(record) => success(id, ResponsePayload::McpServerUpdated(record)),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32021, "mcp_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpRemoveServer(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.remove_server(&actor, &params.name).await {
                            Ok(()) => success(
                                id,
                                ResponsePayload::McpServerRemoved(LegacyOkResult { ok: true }),
                            ),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32021, "mcp_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpConnect(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.connect(&actor, &params.name).await {
                            Ok(result) => success(id, ResponsePayload::McpConnected(result)),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32021, "mcp_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpDisconnect(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.disconnect(&actor, &params.name).await {
                            Ok(result) => success(id, ResponsePayload::McpDisconnected(result)),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32021, "mcp_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpHealth(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => match admin.health(&params.name).await {
                        Ok(result) => success(id, ResponsePayload::McpHealth(result)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32021, "mcp_admin_failed", &message),
                        ),
                    },
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpListTools(_) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => match admin.list_tools().await {
                        Ok(result) => success(id, ResponsePayload::McpToolsListed(result)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32021, "mcp_admin_failed", &message),
                        ),
                    },
                    None => success(
                        id,
                        ResponsePayload::McpToolsListed(McpToolListResult::default()),
                    ),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpReconnectAll(_) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.reconnect_all(&actor).await {
                            Ok(result) => success(id, ResponsePayload::McpReconnected(result)),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32021, "mcp_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpStoreSearch(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => match admin.store_search(&params.query, params.limit).await {
                        Ok(result) => success(id, ResponsePayload::McpStoreSearched(result)),
                        Err(McpStoreError::Unavailable(message)) => failure(
                            id,
                            masked_detail_error(-32020, "mcp_admin_unavailable", &message),
                        ),
                        Err(error) => failure(id, mcp_store_error_frame(error)),
                    },
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpStorePopular(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => match admin.store_popular(params.limit).await {
                        Ok(result) => success(id, ResponsePayload::McpStoreSearched(result)),
                        Err(McpStoreError::Unavailable(message)) => failure(
                            id,
                            masked_detail_error(-32020, "mcp_admin_unavailable", &message),
                        ),
                        Err(error) => failure(id, mcp_store_error_frame(error)),
                    },
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::McpStoreInstall(params) => {
                let frame = match &self.inner.mcp_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.store_install(&actor, &params).await {
                            Ok(result) => success(id, ResponsePayload::McpStoreInstalled(result)),
                            Err(McpStoreError::Unavailable(message)) => failure(
                                id,
                                masked_detail_error(-32020, "mcp_admin_unavailable", &message),
                            ),
                            Err(error) => failure(id, mcp_store_error_frame(error)),
                        }
                    }
                    None => failure(id, error(-32020, "mcp_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SkillsList(params) => {
                let frame = match &self.inner.skill_admin {
                    Some(admin) => match admin.list(params.source.as_deref()).await {
                        Ok(result) => success(id, ResponsePayload::SkillsListed(result)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32023, "skills_admin_failed", &message),
                        ),
                    },
                    None => success(
                        id,
                        ResponsePayload::SkillsListed(SkillListResult::default()),
                    ),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SkillsCreate(params) => {
                let frame = match &self.inner.skill_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.create(&actor, &params).await {
                            Ok(result) => success(id, ResponsePayload::SkillCreated(result)),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32023, "skills_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32022, "skills_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SkillsDelete(params) => {
                let frame = match &self.inner.skill_admin {
                    Some(admin) => {
                        let actor = local_actor().id;
                        match admin.delete(&actor, &params.name).await {
                            Ok(()) => success(
                                id,
                                ResponsePayload::SkillDeleted(LegacyOkResult { ok: true }),
                            ),
                            Err(message) => failure(
                                id,
                                masked_detail_error(-32023, "skills_admin_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32022, "skills_admin_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::ProvidersListModels(params) => {
                let frame = match &self.inner.provider_probe {
                    Some(probe) => match probe.list_models(&local_actor().id, params.id).await {
                        Ok(models) => success(id, ResponsePayload::ProvidersModelsLive(models)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32026, "provider_probe_failed", &message),
                        ),
                    },
                    None => failure(id, error(-32025, "provider_probe_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::ProvidersPreviewModels(params) => {
                let frame = match &self.inner.provider_probe {
                    Some(probe) => match probe
                        .preview_models(&params.name, params.base_url.as_deref(), &params.api_key)
                        .await
                    {
                        Ok(models) => success(id, ResponsePayload::ProvidersModelsPreview(models)),
                        Err(message) => failure(
                            id,
                            masked_detail_error(-32026, "provider_probe_failed", &message),
                        ),
                    },
                    None => failure(id, error(-32025, "provider_probe_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::RssFetchNow(params) => {
                let frame = match &self.inner.rss_feed_fetch {
                    Some(fetch) => {
                        let actor = local_actor();
                        match fetch
                            .fetch_now(&actor.id, params.id, params.idempotency_key.as_str())
                            .await
                        {
                            Ok(result) => success(id, ResponsePayload::RssFetched(result)),
                            Err(RssFetchError::NotFound) => {
                                failure(id, error(-32004, "rss_subscription_not_found", false))
                            }
                            Err(RssFetchError::Conflict(message)) => {
                                failure(id, masked_detail_error(-32006, "conflict", &message))
                            }
                            Err(RssFetchError::Unavailable(message)) => failure(
                                id,
                                masked_detail_error(-32027, "rss_fetch_unavailable", &message),
                            ),
                            Err(RssFetchError::Failed(message)) => failure(
                                id,
                                masked_detail_error(-32028, "rss_fetch_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32027, "rss_fetch_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::MemoryExtract(params) => {
                let frame = match &self.inner.memory_extractor {
                    Some(extractor) => {
                        let actor = local_actor();
                        match extractor
                            .extract(
                                &actor.id,
                                params.conversation_id,
                                params.idempotency_key.as_str(),
                            )
                            .await
                        {
                            Ok(result) => success(id, ResponsePayload::MemoryExtracted(result)),
                            Err(MemoryExtractError::Unavailable(message)) => failure(
                                id,
                                masked_detail_error(-32029, "memory_extract_unavailable", &message),
                            ),
                            Err(MemoryExtractError::Conflict(message)) => {
                                failure(id, masked_detail_error(-32006, "conflict", &message))
                            }
                            Err(MemoryExtractError::Failed(message)) => failure(
                                id,
                                masked_detail_error(-32030, "memory_extract_failed", &message),
                            ),
                        }
                    }
                    None => failure(id, error(-32029, "memory_extract_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::TasksTemplates(_) => {
                let _ = self
                    .send(
                        &outbound,
                        success(id, ResponsePayload::TasksTemplatesListed(task_templates())),
                    )
                    .await;
            }
            Command::TasksRun(params) => {
                let frame = match self.inner.task_executor.as_ref() {
                    Some(executor) => match executor
                        .run_now(&local_actor(), params.id, params.idempotency_key.as_str())
                        .await
                    {
                        Ok(run) => match legacy::convert(run) {
                            Ok(record) => success(id, ResponsePayload::TasksRan(record)),
                            Err(error) => failure(id, error),
                        },
                        Err(error) => failure(id, legacy::store_error(error)),
                    },
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::TasksRunsCancel(params) => {
                let frame = match self.inner.task_executor.as_ref() {
                    Some(executor) => match executor
                        .cancel(&local_actor(), params.id, params.idempotency_key.as_str())
                        .await
                    {
                        Ok(run) => success(
                            id,
                            ResponsePayload::TasksRunsCancelled(TaskRunCancelResult {
                                task_run_id: run.id,
                                cancelled: run.status == "cancelled",
                            }),
                        ),
                        Err(error) => failure(id, legacy::store_error(error)),
                    },
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::TasksScheduler(_) => {
                let frame = match self.inner.task_executor.as_ref() {
                    Some(executor) => match executor.status(&local_actor()) {
                        Ok(status) => success(id, ResponsePayload::TasksScheduler(status)),
                        Err(error) => failure(id, legacy::store_error(error)),
                    },
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SubagentsLaunch(params) => {
                let frame = match self.inner.subagent_executor.as_ref() {
                    Some(executor) => {
                        let spec = SubagentLaunchSpec {
                            parent_conversation_id: params.parent_conversation_id,
                            role_id: params.role_id,
                            profile_id: params.profile_id,
                            parent_run_id: params.parent_run_id,
                            research_run_id: None,
                            name: params.name.clone(),
                            prompt: params.prompt.clone(),
                            model: params.model.clone(),
                        };
                        let fingerprint = legacy::fingerprint(&params);
                        match executor
                            .launch(
                                &local_actor().id,
                                spec,
                                params.idempotency_key.as_str(),
                                &fingerprint,
                            )
                            .await
                        {
                            Ok(run) => match legacy::convert(run) {
                                Ok(record) => {
                                    success(id, ResponsePayload::SubagentsLaunched(record))
                                }
                                Err(error) => failure(id, error),
                            },
                            Err(error) => failure(id, legacy::store_error(error)),
                        }
                    }
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SubagentsLaunchBatch(params) => {
                let frame = match self.inner.subagent_executor.as_ref() {
                    Some(executor) => {
                        let specs = params
                            .items
                            .iter()
                            .map(|item| SubagentLaunchSpec {
                                parent_conversation_id: params.parent_conversation_id,
                                role_id: item.role_id,
                                profile_id: item.profile_id,
                                parent_run_id: None,
                                research_run_id: None,
                                name: item.name.clone(),
                                prompt: item.prompt.clone(),
                                model: item.model.clone(),
                            })
                            .collect::<Vec<_>>();
                        let fingerprint = legacy::fingerprint(&params);
                        match executor
                            .launch_batch(
                                &local_actor().id,
                                specs,
                                params.idempotency_key.as_str(),
                                &fingerprint,
                            )
                            .await
                        {
                            Ok(runs) => match legacy::convert(runs) {
                                Ok(records) => {
                                    success(id, ResponsePayload::SubagentsLaunchedBatch(records))
                                }
                                Err(error) => failure(id, error),
                            },
                            Err(error) => failure(id, legacy::store_error(error)),
                        }
                    }
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::SubagentsRunsCancel(params) => {
                let frame = match self.inner.subagent_executor.as_ref() {
                    Some(executor) => {
                        let fingerprint = legacy::fingerprint(&params);
                        match executor
                            .cancel(
                                &local_actor().id,
                                params.id,
                                params.idempotency_key.as_str(),
                                &fingerprint,
                            )
                            .await
                        {
                            Ok(run) => success(
                                id,
                                ResponsePayload::SubagentsRunsCancelled(SubagentRunCancelResult {
                                    run_id: run.id,
                                    cancelled: run.status == "cancelled",
                                }),
                            ),
                            Err(error) => failure(id, legacy::store_error(error)),
                        }
                    }
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::WebhooksReplay(params) => {
                let frame = match (
                    self.inner.config.legacy_store.as_deref(),
                    self.inner.task_executor.as_ref(),
                ) {
                    (Some(store), Some(executor)) => {
                        let actor = local_actor();
                        let fingerprint = legacy::fingerprint(&params);
                        match store
                            .run_idempotent_async(
                                &actor.id,
                                "webhooks.replay",
                                params.idempotency_key.as_str(),
                                &fingerprint,
                                || {
                                    replay_webhook(
                                        store,
                                        executor,
                                        &actor,
                                        params.event_id,
                                        params.endpoint_id,
                                    )
                                },
                            )
                            .await
                        {
                            Ok(outcome) => match legacy::convert(outcome.value) {
                                Ok(record) => {
                                    success(id, ResponsePayload::WebhooksReplayed(record))
                                }
                                Err(error) => failure(id, error),
                            },
                            Err(error) => failure(id, legacy::store_error(error)),
                        }
                    }
                    _ => failure(id, error(-32010, "legacy_store_unavailable", false)),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::PlansExecute(params) => {
                let frame = match self
                    .start_plan_execution(&params, &outbound, &connection)
                    .await
                {
                    Ok(result) => success(id, ResponsePayload::PlansExecuted(result)),
                    Err(error) => failure(id, error),
                };
                let _ = self.send(&outbound, frame).await;
            }
            Command::ConversationsCompact(params) => {
                let frame = match self.inner.config.legacy_store.as_deref() {
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                    Some(legacy) => {
                        let actor = local_actor();
                        // Resolve the canonical session before the mutation so
                        // the idempotent closure only writes.
                        let session_id = match self.ensure_conversation_session(params.id) {
                            Ok(session_id) => session_id,
                            Err(error) => {
                                let _ = self.send(&outbound, failure(id, error)).await;
                                return;
                            }
                        };
                        let fingerprint = legacy::fingerprint(&params);
                        match legacy
                            .run_idempotent_async(
                                &actor.id,
                                "conversations.compact",
                                params.idempotency_key.as_str(),
                                &fingerprint,
                                || async {
                                    self.compact_conversation(&actor, params.id, &session_id)
                                        .await
                                },
                            )
                            .await
                        {
                            Ok(outcome) => match legacy::convert(outcome.value) {
                                Ok(record) => {
                                    success(id, ResponsePayload::ConversationsCompacted(record))
                                }
                                Err(error) => failure(id, error),
                            },
                            Err(error) => failure(id, legacy::store_error(error)),
                        }
                    }
                };
                let _ = self.send(&outbound, frame).await;
            }
            command => {
                let frame = match self.inner.config.legacy_store.as_deref() {
                    None => failure(id, error(-32010, "legacy_store_unavailable", false)),
                    Some(store) => {
                        let actor = local_actor();
                        match legacy::dispatch(
                            store,
                            self.inner.config.secrets.as_deref(),
                            self.inner.workspace.root(),
                            &actor,
                            command,
                        )
                        .await
                        {
                            Ok(payload) => {
                                // Post-dispatch kickoff: `research.create`/
                                // `research.rerun` persist the row; here the
                                // canonical run starts and the client learns
                                // which run to stream via `runtime_run_id`.
                                match payload {
                                    created @ (ResponsePayload::ResearchCreated(_)
                                    | ResponsePayload::ResearchReran(_)) => {
                                        let (mut record, is_rerun) = match created {
                                            ResponsePayload::ResearchCreated(record) => {
                                                (record, false)
                                            }
                                            ResponsePayload::ResearchReran(record) => {
                                                (record, true)
                                            }
                                            _ => unreachable!(),
                                        };
                                        let runtime = match record.conversation_id {
                                            Some(conversation_id) => {
                                                self.start_research_execution(
                                                    record.id,
                                                    conversation_id,
                                                    &outbound,
                                                    &connection,
                                                )
                                                .await
                                            }
                                            None => Ok(None),
                                        };
                                        match runtime {
                                            Ok(run_id) => {
                                                record.runtime_run_id = run_id;
                                                let payload = if is_rerun {
                                                    ResponsePayload::ResearchReran(record)
                                                } else {
                                                    ResponsePayload::ResearchCreated(record)
                                                };
                                                success(id, payload)
                                            }
                                            Err(error) => failure(id, error),
                                        }
                                    }
                                    ResponsePayload::ResearchCancelled(result) => {
                                        if let Some(executor) =
                                            self.inner.research_executor.as_ref()
                                        {
                                            executor.signal_cancel(result.cancelled);
                                        }
                                        success(id, ResponsePayload::ResearchCancelled(result))
                                    }
                                    other => success(id, other),
                                }
                            }
                            Err(error) => failure(id, error),
                        }
                    }
                };
                let _ = self.send(&outbound, frame).await;
            }
        }
    }

    fn transport_limits(&self) -> TransportLimits {
        TransportLimits {
            max_frame_bytes: self.inner.config.max_frame_bytes as u32,
            max_rpc_id_bytes: MAX_RPC_ID_BYTES as u16,
            max_in_flight: self.inner.config.max_in_flight as u16,
            outbound_queue: self.inner.config.outbound_queue as u16,
            event_page_limit: self.inner.config.event_page_limit,
        }
    }

    /// Project the runtime's registered tools into the protocol catalog record.
    /// `is_macro` mirrors the legacy convention that macro-backed tools are
    /// named with a `macro_` prefix.
    fn tool_catalog(&self) -> Vec<ToolCatalogRecord> {
        self.inner
            .runtime
            .tool_catalog()
            .into_iter()
            .map(|entry| ToolCatalogRecord {
                is_macro: entry.name.starts_with("macro_"),
                name: entry.name,
                description: entry.description,
                dangerous: entry.dangerous,
                capabilities: entry.capabilities,
                parameters: entry.parameters,
            })
            .collect()
    }

    async fn send(&self, outbound: &Outbound, frame: ServerFrame) -> bool {
        outbound.send(frame).await
    }

    /// Register `connection_id` as the owner of a live run so `run.subscribe`
    /// fan-out can avoid double-delivering to the owner's run sink.
    async fn register_run_owner(&self, run_id: &str, connection_id: &str) {
        self.inner
            .run_owners
            .lock()
            .await
            .insert(run_id.to_owned(), connection_id.to_owned());
    }

    /// Subscribe one connection to a live run's events.
    async fn register_subscriber(&self, run_id: &str, connection_id: &str, outbound: &Outbound) {
        self.inner
            .run_subscribers
            .lock()
            .await
            .entry(run_id.to_owned())
            .or_default()
            .insert(connection_id.to_owned(), outbound.clone());
    }

    /// Remove one connection from a single run's subscriber set.
    async fn unregister_subscriber(&self, run_id: &str, connection_id: &str) {
        let mut subscribers = self.inner.run_subscribers.lock().await;
        if let Some(entries) = subscribers.get_mut(run_id) {
            entries.remove(connection_id);
            if entries.is_empty() {
                subscribers.remove(run_id);
            }
        }
    }

    /// Remove one connection's live-run registrations on disconnect.
    async fn drop_connection_registrations(
        &self,
        connection_id: &str,
        subscribed: &HashSet<String>,
    ) {
        {
            let mut owners = self.inner.run_owners.lock().await;
            owners.retain(|_, owner| owner != connection_id);
        }
        if subscribed.is_empty() {
            return;
        }
        let mut subscribers = self.inner.run_subscribers.lock().await;
        for run_id in subscribed {
            if let Some(entries) = subscribers.get_mut(run_id) {
                entries.remove(connection_id);
                if entries.is_empty() {
                    subscribers.remove(run_id);
                }
            }
        }
    }

    /// Fan a durable event out to every `run.subscribe` connection except the
    /// one that owns the run (which receives it through its own run sink).
    async fn publish_to_subscribers(&self, envelope: &EventEnvelope) {
        let targets = {
            let owner = self
                .inner
                .run_owners
                .lock()
                .await
                .get(&envelope.run_id)
                .cloned();
            let subscribers = self.inner.run_subscribers.lock().await;
            subscribers
                .get(&envelope.run_id)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|(connection_id, _)| {
                            Some(connection_id.as_str()) != owner.as_deref()
                        })
                        .map(|(_, outbound)| outbound.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        for outbound in targets {
            let _ = self.send(&outbound, notification(envelope.clone())).await;
        }
        if matches!(
            envelope.event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        ) {
            self.inner
                .run_subscribers
                .lock()
                .await
                .remove(&envelope.run_id);
            self.inner.run_owners.lock().await.remove(&envelope.run_id);
        }
    }

    fn prompt_start_frames_fit(
        &self,
        session_id: &str,
        content: &str,
        model: Option<&str>,
    ) -> bool {
        [
            CanonicalEvent::RunStarted(RunStarted {
                model: model.map(str::to_owned),
                mode: Some("m7_rust_agent".to_owned()),
            }),
            CanonicalEvent::ContentDelta(TextDelta {
                text: content.to_owned(),
                channel: Some("final".to_owned()),
            }),
            CanonicalEvent::RunCompleted(RunTerminal {
                reason: "stop".to_owned(),
                error_code: None,
            }),
        ]
        .into_iter()
        .all(|event| self.preview_event_frame_fits(session_id, event))
    }

    async fn run_event_frame_fits(&self, run_id: &str, event: CanonicalEvent) -> bool {
        let Ok(run) = self.inner.store.run(run_id, &local_actor().id) else {
            return true;
        };
        self.preview_event_frame_fits(&run.session_id, event)
    }

    fn preview_event_frame_fits(&self, session_id: &str, event: CanonicalEvent) -> bool {
        let envelope = preview_event_envelope(session_id, event);
        let notification_fits = serde_json::to_vec(&notification(envelope.clone()))
            .is_ok_and(|encoded| encoded.len() <= self.inner.config.max_frame_bytes);
        let replay_fits = serde_json::to_vec(&preview_event_page(envelope, false))
            .is_ok_and(|encoded| encoded.len() <= self.inner.config.max_frame_bytes);
        notification_fits && replay_fits
    }

    async fn create_session(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: String,
        title: Option<String>,
        project_key: Option<String>,
    ) -> Result<String, ProtocolError> {
        self.inner
            .store
            .create_session(
                actor_id,
                key,
                &fingerprint,
                title.as_deref(),
                project_key.as_deref(),
            )
            .map(|outcome| outcome.value)
            .map_err(store_error)
    }

    async fn load_session(&self, session_id: &str) -> Option<SessionLoadedResult> {
        let session = self
            .inner
            .store
            .load_session(session_id, &local_actor().id)
            .ok()?;
        Some(SessionLoadedResult {
            session_id: session_id.to_owned(),
            active_run_id: session.active_run_id,
            last_seq: session.last_seq,
        })
    }

    fn session_list(
        &self,
        project_key: Option<&str>,
        limit: u16,
    ) -> Result<SessionListResult, ProtocolError> {
        if limit == 0 || limit > self.inner.config.event_page_limit {
            return Err(error(-32602, "invalid_session_list_limit", false));
        }
        let sessions = self
            .inner
            .store
            .list_sessions(&local_actor().id, project_key, usize::from(limit))
            .map_err(store_error)?;
        Ok(SessionListResult {
            sessions: sessions
                .into_iter()
                .map(|session| SessionSummary {
                    session_id: session.session_id,
                    title: session.title,
                    project_key: session.project_key,
                    active_run_id: session.active_run_id,
                    last_seq: session.last_seq,
                    created_at: session.created_at,
                })
                .collect(),
        })
    }

    fn session_history(
        &self,
        response_id: &RpcId,
        session_id: &str,
        limit: u16,
        before_cursor: Option<u64>,
    ) -> Result<SessionHistoryResult, ProtocolError> {
        if limit == 0 || limit > self.inner.config.event_page_limit {
            return Err(error(-32602, "invalid_session_history_limit", false));
        }
        // Walk events newest-first in bounded chunks. A page must begin at a
        // history-group boundary (an item/tool event, not a reasoning delta),
        // otherwise the oldest group's reasoning would be cut off and the
        // following page could never recover it. Reading continues until the
        // window starts at such a boundary or the log is exhausted.
        let requested = usize::from(limit);
        let mut cursor = before_cursor;
        let mut window: Vec<(u64, EventEnvelope)> = Vec::new();
        // A single item can be preceded by many reasoning/content deltas, so
        // reading by event needs more than `limit` rows; the cap turns a
        // pathological unbroken delta run into a structured error instead of
        // silently dropping content.
        let max_events = requested.saturating_mul(64).max(256);
        let exhausted = loop {
            let chunk = self
                .inner
                .store
                .session_event_window(session_id, &local_actor().id, cursor, requested + 1)
                .map_err(store_error)?;
            if chunk.is_empty() {
                break true;
            }
            cursor = Some(chunk[0].0);
            let exhausted = chunk.len() <= requested;
            window.splice(0..0, chunk);
            // Reasoning and content deltas attach to the item that follows
            // them, so the window is only group-complete once its oldest event
            // is not a delta. An assistant item as the oldest event means its
            // leading reasoning deltas are still older, so the walk must
            // continue past it as well (otherwise that reasoning would be
            // split onto no page at all).
            let starts_at_boundary = !matches!(
                window[0].1.event,
                CanonicalEvent::ReasoningDelta(_)
                    | CanonicalEvent::ContentDelta(_)
                    | CanonicalEvent::UsageUpdated(_)
            ) && !matches!(
                &window[0].1.event,
                CanonicalEvent::ItemCompleted(item)
                    if item.role.as_deref() == Some("assistant")
            );
            if starts_at_boundary || exhausted {
                break exhausted;
            }
            if window.len() >= max_events {
                return Err(error(-32008, "session_history_scan_limit", false));
            }
        };
        // A bounded page can start mid-run, so the run's `run.started` event
        // (and its model) may be older than the window. Page-contained starts
        // are free; for the rest resolve the model with one bounded read per
        // distinct run.
        let actor = local_actor();
        let mut models = std::collections::HashMap::new();
        for (_, envelope) in &window {
            if let CanonicalEvent::RunStarted(started) = &envelope.event
                && let Some(model) = started.model.as_ref()
            {
                models.insert(envelope.run_id.clone(), model.clone());
            }
        }
        let mut run_ids = window
            .iter()
            .map(|(_, envelope)| envelope.run_id.clone())
            .collect::<Vec<_>>();
        run_ids.sort();
        run_ids.dedup();
        for run_id in &run_ids {
            if models.contains_key(run_id) {
                continue;
            }
            if let Ok(events) = self.inner.store.events(run_id, &actor.id, None, 1)
                && let Some(CanonicalEvent::RunStarted(started)) =
                    events.first().map(|envelope| &envelope.event)
                && let Some(model) = started.model.as_ref()
            {
                models.insert(run_id.clone(), model.clone());
            }
        }
        let entries = history_entries_from_window(&window, &models);
        // `exhausted` means no older events exist; otherwise older events (and
        // therefore possibly older items) remain.
        let has_more = !exhausted || entries.len() > requested;
        bounded_history(
            entries,
            requested,
            has_more,
            self.inner.config.max_frame_bytes,
            response_id,
        )
    }

    async fn start_prompt(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: String,
        session_id: &str,
    ) -> Result<(String, watch::Receiver<Option<String>>, bool), ProtocolError> {
        let outcome = self
            .inner
            .store
            .start_run(actor_id, key, &fingerprint, session_id)
            .map_err(store_error)?;
        let run_id = outcome.value;
        if !outcome.created {
            let state = self.inner.state.lock().await;
            if let Some(run) = state.runs.get(&run_id) {
                return Ok((run_id, run.cancel.subscribe(), false));
            }
            let (_sender, receiver) = watch::channel(Some("durable_replay".to_owned()));
            return Ok((run_id, receiver, false));
        }
        let (cancel, receiver) = watch::channel(None);
        let mut state = self.inner.state.lock().await;
        state.runs.insert(
            run_id.clone(),
            RunRecord {
                cancel,
                terminal: false,
            },
        );
        state.prompt_executions += 1;
        Ok((run_id, receiver, true))
    }

    async fn existing_prompt(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
    ) -> Result<Option<String>, ProtocolError> {
        self.inner
            .store
            .lookup_idempotent(actor_id, "session.prompt", key, fingerprint)
            .map_err(store_error)
    }

    fn spawn_agent_run(
        &self,
        run_id: String,
        prompt: PromptRequest,
        cancel: watch::Receiver<Option<String>>,
        outbound: Outbound,
    ) {
        let server = self.clone();
        tokio::spawn(async move {
            let Some(run) = server.inner.store.run(&run_id, &local_actor().id).ok() else {
                return;
            };
            let sink = AppServerEventSink {
                server: server.clone(),
                run_id: run_id.clone(),
                outbound: outbound.clone(),
                steer_cursor: Arc::new(AtomicU64::new(run.last_seq)),
                own_user_items: Arc::new(Mutex::new(HashSet::new())),
            };
            let approvals = AppServerApprovalGate {
                server: server.clone(),
                run_id: run_id.clone(),
                session_id: run.session_id.clone(),
                outbound: outbound.clone(),
            };
            let masked_prompt = mask_secrets(&prompt.content);
            // Planning mode owns the system prompt: a caller cannot override the
            // directive that turns the turn into plan generation. The prompt is
            // used only for the model request and is never persisted, so it is
            // not masked here (matching the user content, which the event sink
            // masks before it reaches the log).
            let (system_prompt, mode) = if prompt.plan_mode {
                (
                    Some(planning_system_prompt().to_owned()),
                    Some("plan".to_owned()),
                )
            } else {
                // A normal turn uses the caller's prompt when present, otherwise
                // the persisted default (empty means no system message).
                let system_prompt = match prompt.system_prompt {
                    Some(system_prompt) => Some(system_prompt),
                    None => default_system_prompt(&server).await,
                };
                (system_prompt, None)
            };
            let request = AgentRequest {
                model: prompt
                    .model
                    .unwrap_or_else(|| server.inner.default_model.clone()),
                history: Vec::<Message>::new(),
                user_input: prompt.content,
                system_prompt,
                mode,
                temperature: 0.0,
                max_tokens: None,
                limits: AgentLimits::default(),
                tool_names: None,
                tool_context: ToolContext::new(
                    server.inner.workspace.clone(),
                    server.inner.policy.clone(),
                )
                .with_actor(local_actor().id)
                .with_conversation(
                    server
                        .inner
                        .store
                        .conversation_id_for_session(&local_actor().id, &run.session_id)
                        .ok()
                        .flatten(),
                ),
            };
            let lifecycle_sink =
                server
                    .inner
                    .lifecycle
                    .as_ref()
                    .map(|lifecycle| LifecycleEventSink {
                        inner: sink.clone(),
                        lifecycle: lifecycle.clone(),
                        policy: server.inner.policy.clone(),
                        prompt: masked_prompt,
                    });
            let event_sink: &dyn EventSink = lifecycle_sink
                .as_ref()
                .map(|value| value as &dyn EventSink)
                .unwrap_or(&sink);
            let result = server
                .inner
                .runtime
                .run(
                    request,
                    event_sink,
                    &approvals,
                    CancelSignal::from_receiver(cancel),
                )
                .await;
            let terminal = server
                .inner
                .store
                .run(&run_id, &local_actor().id)
                .is_ok_and(|run| run.status.is_terminal());
            if result.is_err() && !terminal {
                server
                    .finish_cancelled(&run_id, "disconnect", Some(&outbound))
                    .await;
            }
            if matches!(
                result,
                Ok(RunOutcome::Completed { .. }
                    | RunOutcome::Cancelled { .. }
                    | RunOutcome::Failed { .. })
            ) && let Some(record) = server.inner.state.lock().await.runs.get_mut(&run_id)
            {
                record.terminal = true;
            }
        });
    }

    /// Start a canonical plan execution: resolve the conversation's session,
    /// open a durable run, register it for cancellation/subscription, and spawn
    /// the step loop. Idempotent through `start_run`'s `(actor, key)` record.
    async fn start_plan_execution(
        &self,
        params: &IdempotentPlanIdParams,
        outbound: &Outbound,
        connection: &Arc<Mutex<ConnectionState>>,
    ) -> Result<PlanExecuteResult, ProtocolError> {
        let actor = local_actor();
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return Err(error(-32010, "legacy_store_unavailable", false));
        };
        let plan = legacy
            .get_plan(&actor.id, params.conversation_id, params.plan_id)
            .map_err(legacy::store_error)?;
        let key = params.idempotency_key.as_str();
        let fingerprint = fingerprint(params);
        // A replay of the same key returns the original run before the
        // `approved` guard (the plan is `executing` by then).
        if let Ok(Some(run_id)) = self.inner.store.lookup_idempotent::<String>(
            &actor.id,
            "session.prompt",
            key,
            &fingerprint,
        ) {
            let status = self
                .inner
                .store
                .run(&run_id, &actor.id)
                .map(|run| run.status.as_str().to_owned())
                .unwrap_or_else(|_| "running".to_owned());
            return Ok(PlanExecuteResult {
                plan_id: plan.id,
                run_id,
                status,
            });
        }
        if plan.status != "approved" {
            return Err(legacy::invalid_input(format!(
                "plan {} is not approved",
                plan.id
            )));
        }
        let steps = legacy
            .list_plan_steps(plan.id)
            .map_err(legacy::store_error)?;
        let session_id = self.ensure_conversation_session(params.conversation_id)?;
        let outcome = self
            .inner
            .store
            .start_run(&actor.id, key, &fingerprint, &session_id)
            .map_err(store_error)?;
        let run_id = outcome.value;
        if !outcome.created {
            let status = self
                .inner
                .store
                .run(&run_id, &actor.id)
                .map(|run| run.status.as_str().to_owned())
                .unwrap_or_else(|_| "running".to_owned());
            return Ok(PlanExecuteResult {
                plan_id: plan.id,
                run_id,
                status,
            });
        }
        // Transition to `executing` synchronously before spawning: a later
        // `plans.execute` for the same plan is then rejected instead of running
        // a second time (e.g. after a cancel cleared the active-run guard).
        legacy
            .set_plan_status(&actor.id, params.conversation_id, plan.id, "executing")
            .map_err(legacy::store_error)?;
        let (cancel, receiver) = watch::channel(None);
        self.inner.state.lock().await.runs.insert(
            run_id.clone(),
            RunRecord {
                cancel,
                terminal: false,
            },
        );
        let connection_id = connection.lock().await.id.clone();
        self.inner
            .run_owners
            .lock()
            .await
            .insert(run_id.clone(), connection_id);
        let plan_id = plan.id;
        let server = self.clone();
        let outbound = outbound.clone();
        let spawned = run_id.clone();
        tokio::spawn(async move {
            server
                .run_plan(spawned.clone(), plan, steps, outbound, receiver)
                .await;
            if let Some(record) = server.inner.state.lock().await.runs.get_mut(&spawned) {
                record.terminal = true;
            }
        });
        Ok(PlanExecuteResult {
            plan_id,
            run_id,
            status: "running".to_owned(),
        })
    }

    /// Find-or-create the canonical session for a conversation using a stable
    /// key, importing the legacy transcript once (same projection as
    /// `session.for_conversation`).
    fn ensure_conversation_session(&self, conversation_id: i64) -> Result<String, ProtocolError> {
        let actor = local_actor();
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return Err(error(-32010, "legacy_store_unavailable", false));
        };
        let key = format!("plan-execute-link:{conversation_id}");
        let fingerprint = format!("plan-execute-link:{conversation_id}");
        if let Ok(Some(link)) = self.inner.store.lookup_idempotent::<ConversationLink>(
            &actor.id,
            "session.for_conversation",
            &key,
            &fingerprint,
        ) {
            return Ok(link.session_id);
        }
        let conversation = legacy
            .get_conversation(&actor.id, conversation_id)
            .map_err(legacy::store_error)?;
        let window = legacy
            .recent_messages(&actor.id, conversation_id, MAX_IMPORTED_MESSAGES)
            .map_err(legacy::store_error)?;
        let mut messages = window.messages;
        let trimmed = trim_orphan_tool_rows(&mut messages);
        let truncated = window.has_more || trimmed > 0;
        let history = legacy_history_events(&messages);
        let link = self
            .inner
            .store
            .link_conversation(
                &actor.id,
                &key,
                &fingerprint,
                conversation_id,
                conversation.title.as_deref(),
                conversation.working_directory.as_deref(),
                &history,
                truncated,
            )
            .map_err(store_error)?;
        Ok(link.session_id)
    }

    /// Compact a conversation: summarize the older canonical items with the
    /// provider runtime, persist the legacy `working_memory` (parity) and append
    /// a canonical `session.compacted` event so the chat can render the rolling
    /// summary without the legacy detail endpoint.
    async fn compact_conversation(
        &self,
        actor: &ActorRef,
        conversation_id: i64,
        session_id: &str,
    ) -> Result<CompactResult, cool_store::StoreError> {
        /// Python `memory_summary_threshold_messages` default (compaction runs
        /// when the item count is below it).
        const THRESHOLD: usize = 30;
        /// Python `keep_recent` (hardcoded in the compact endpoint).
        const KEEP: usize = 10;
        let legacy = self
            .inner
            .config
            .legacy_store
            .as_deref()
            .ok_or(cool_store::StoreError::NotALegacyStore)?;
        let mut items = self.collect_session_items(session_id)?;
        // The previous rolling summary/cutoff live on the canonical summary
        // item (newest wins); the legacy message-id column is a different id
        // space and is not a valid cursor.
        let previous = items
            .iter()
            .filter(|item| item.role == "summary")
            .max_by_key(|item| item.cursor)
            .map(|item| (item.content.clone(), item.compact_up_to_cursor));
        items.retain(|item| item.role != "summary");
        let message_count = items.len() as u64;
        if items.len() < THRESHOLD {
            return Ok(CompactResult {
                status: "skipped".to_owned(),
                reason: Some(format!("Too few messages ({message_count} < {THRESHOLD})")),
                message_count: Some(message_count),
                messages_compacted: None,
                messages_kept: None,
                summary_length: None,
            });
        }
        let compacted = &items[..items.len() - KEEP];
        let cutoff = compacted.last().map(|item| item.cursor);
        let previous_cutoff = previous.as_ref().and_then(|(_, cutoff)| *cutoff);
        let previous_summary = previous.as_ref().and_then(|(summary, _)| summary.clone());
        let fresh = compacted
            .iter()
            .filter(|item| previous_cutoff.is_none_or(|cutoff| item.cursor > cutoff))
            .collect::<Vec<_>>();
        if fresh.is_empty() {
            return Ok(CompactResult {
                status: "skipped".to_owned(),
                reason: Some("No new messages to compact".to_owned()),
                message_count: Some(message_count),
                messages_compacted: None,
                messages_kept: Some(KEEP as u64),
                summary_length: None,
            });
        }
        let mut transcript = String::new();
        if let Some(summary) = &previous_summary {
            transcript.push_str("[Previous summary of the earlier conversation]\n");
            transcript.push_str(summary);
            transcript.push('\n');
        }
        for item in &fresh {
            let content = item.content.clone().unwrap_or_default();
            transcript.push_str(&format!(
                "{}: {}\n",
                item.role,
                truncate_chars(&content, 300)
            ));
        }
        let model = legacy
            .get_conversation(&actor.id, conversation_id)
            .ok()
            .and_then(|conversation| conversation.model)
            .unwrap_or_else(|| self.inner.default_model.clone());
        // Mask before truncating so a secret cannot be split across a cut and
        // survive the pattern matcher; a provider failure falls back to a
        // deterministic extractive summary so compaction never loses context.
        let fallback = truncate_chars(&mask_secrets(&transcript), 4000);
        let summary = self
            .summarize_conversation(&transcript, &model)
            .await
            .map(|text| truncate_chars(&text, 8000))
            .unwrap_or(fallback);
        let summary_length = summary.chars().count() as i64;
        // Project the canonical summary first: if it fails, fail the command
        // (leaving the legacy working memory untouched) rather than advancing
        // one store without the other.
        if let Some(cutoff) = cutoff {
            self.project_compaction(&actor.id, session_id, KEEP as u32, &summary, cutoff)
                .map_err(|error| cool_store::StoreError::Corruption(error.to_string()))?;
        }
        let existing = legacy.get_working_memory(&actor.id, conversation_id)?;
        let state = existing
            .as_ref()
            .map(|memory| memory.state.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        // The canonical summary item is the source of truth for freshness, so
        // the legacy message-id cutoff column is intentionally left unset.
        legacy.upsert_working_memory(
            &actor.id,
            conversation_id,
            &state,
            Some(&summary),
            None,
            Some(summary_length),
        )?;
        Ok(CompactResult {
            status: "compacted".to_owned(),
            reason: None,
            message_count: Some(message_count),
            messages_compacted: Some(fresh.len() as u64),
            messages_kept: Some(KEEP as u64),
            summary_length: Some(summary_length as u64),
        })
    }

    /// Whole chronological canonical transcript (bounded), used by compaction.
    fn collect_session_items(
        &self,
        session_id: &str,
    ) -> Result<Vec<HistoryItem>, cool_store::StoreError> {
        let limit = self.inner.config.event_page_limit.min(100);
        let mut items = Vec::new();
        let mut cursor = None;
        for _ in 0..64 {
            let page = self
                .session_history(&RpcId::Null, session_id, limit, cursor)
                .map_err(|_| {
                    cool_store::StoreError::Corruption("session history read".to_owned())
                })?;
            let has_more = page.has_more;
            cursor = page.next_cursor;
            items.extend(page.items);
            if !has_more || cursor.is_none() {
                break;
            }
        }
        items.sort_by_key(|item| item.cursor);
        Ok(items)
    }

    /// One provider call producing the rolling summary (masked).
    async fn summarize_conversation(&self, transcript: &str, model: &str) -> Option<String> {
        let request = AgentRequest {
            model: model.to_owned(),
            history: Vec::new(),
            user_input: transcript.to_owned(),
            system_prompt: Some(SUMMARIZER_SYSTEM_PROMPT.to_owned()),
            mode: Some("compact".to_owned()),
            temperature: 0.0,
            max_tokens: Some(1000),
            limits: AgentLimits {
                max_iterations: 1,
                ..AgentLimits::default()
            },
            tool_names: Some(BTreeSet::new()),
            tool_context: ToolContext::new(self.inner.workspace.clone(), self.inner.policy.clone())
                .with_actor(local_actor().id),
        };
        let sink = PlanStepSink::default();
        let (_sender, signal) = CancelSignal::channel();
        let outcome = self
            .inner
            .runtime
            .run(
                request,
                &sink,
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
                signal,
            )
            .await
            .ok()?;
        if !matches!(outcome, RunOutcome::Completed { .. }) {
            return None;
        }
        let text = mask_secrets(sink.text().trim());
        (!text.is_empty()).then_some(text)
    }

    /// Append the canonical `session.compacted` projection on an auxiliary run.
    fn project_compaction(
        &self,
        actor_id: &str,
        session_id: &str,
        retained: u32,
        summary: &str,
        cutoff: u64,
    ) -> Result<(), cool_state::StoreError> {
        let run_id = self.inner.store.start_auxiliary_run(actor_id, session_id)?;
        let envelope = |event: CanonicalEvent| EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.to_owned(),
            run_id: run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: rfc3339_now(),
            actor: ActorRef {
                id: "cool-agent".to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-app-server-compact".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: Default::default(),
        };
        let compacted = envelope(CanonicalEvent::SessionCompacted(SessionCompacted {
            retained_items: retained,
            summary_item_id: None,
            summary: Some(summary.to_owned()),
            compact_up_to_cursor: Some(cutoff),
        }));
        self.inner.store.append_event_auto(actor_id, compacted)?;
        let terminal = envelope(CanonicalEvent::RunCompleted(RunTerminal {
            reason: "compact".to_owned(),
            error_code: None,
        }));
        self.inner.store.append_event_auto(actor_id, terminal)?;
        Ok(())
    }

    /// Execute an approved plan's steps, emitting canonical `plan.*` events into
    /// the durable run and finalizing the plan and run.
    async fn run_plan(
        &self,
        run_id: String,
        plan: cool_store::domains::plans::Plan,
        steps: Vec<cool_store::domains::plans::PlanStep>,
        outbound: Outbound,
        cancel_rx: watch::Receiver<Option<String>>,
    ) {
        let actor = local_actor();
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return;
        };
        let sink = AppServerEventSink {
            server: self.clone(),
            run_id: run_id.clone(),
            outbound: outbound.clone(),
            steer_cursor: Arc::new(AtomicU64::new(0)),
            own_user_items: Arc::new(Mutex::new(HashSet::new())),
        };
        let ordered = topological_order(&steps);
        let total = ordered.len() as u32;
        let _ = sink
            .emit(CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: Some("plan".to_owned()),
            }))
            .await;
        let _ = sink
            .emit(CanonicalEvent::PlanCreated(PlanCreated {
                plan_id: plan.id.to_string(),
                title: plan.title.clone(),
                total_steps: total,
                steps: ordered
                    .iter()
                    .map(|step| protocol_plan_step(&plan, step, &step.status, None))
                    .collect(),
                store_plan_id: Some(plan.id),
            }))
            .await;
        let _ = sink
            .emit(CanonicalEvent::PlanProgress(PlanProgress {
                plan_id: plan.id.to_string(),
                completed_steps: 0,
                total_steps: total,
                message: None,
                status: PlanProgressStatus::Executing,
            }))
            .await;
        let conversation = legacy
            .get_conversation(&actor.id, plan.conversation_id)
            .ok();
        let model = conversation
            .as_ref()
            .and_then(|conversation| conversation.model.clone())
            .unwrap_or_else(|| self.inner.default_model.clone());
        let workspace = match conversation
            .as_ref()
            .and_then(|conversation| conversation.working_directory.as_deref())
        {
            Some(path) => match Workspace::new(path) {
                Ok(workspace) => workspace,
                Err(_) => {
                    let _ = self
                        .finish_plan(&sink, legacy, &actor, &plan, "failed", 0, total)
                        .await;
                    return;
                }
            },
            None => self.inner.workspace.clone(),
        };
        let mut statuses = steps
            .iter()
            .map(|step| (step.position, step.status.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut history = plan_history(legacy, &actor.id, plan.conversation_id);
        let mut completed = 0_u32;
        let mut failed = false;
        let mut cancelled = false;
        for step in &ordered {
            if cancel_rx.borrow().is_some() || plan_is_cancelled(legacy, &actor, &plan) {
                cancelled = true;
                break;
            }
            if !dependencies_met(step, &statuses) {
                statuses.insert(step.position, "skipped".to_owned());
                let _ = legacy.update_plan_step_status(
                    plan.id,
                    step.position,
                    "skipped",
                    Some("Skipped: dependencies not met"),
                    None,
                );
                let _ = sink
                    .emit(CanonicalEvent::PlanStepCompleted(protocol_plan_step(
                        &plan,
                        step,
                        "skipped",
                        Some("Skipped: dependencies not met"),
                    )))
                    .await;
                continue;
            }
            statuses.insert(step.position, "running".to_owned());
            let _ = legacy.update_plan_step_status(plan.id, step.position, "running", None, None);
            let _ = sink
                .emit(CanonicalEvent::PlanStepStarted(protocol_plan_step(
                    &plan, step, "running", None,
                )))
                .await;
            let (summary, step_failed) = self
                .execute_plan_step(&plan, step, &history, &workspace, &model, &cancel_rx)
                .await;
            let summary = mask_secrets(&summary);
            if step_failed {
                // A step that stopped because the plan was cancelled is not a
                // step failure; the plan and its run are cancelled instead.
                if cancel_rx.borrow().is_some() || plan_is_cancelled(legacy, &actor, &plan) {
                    cancelled = true;
                    break;
                }
                statuses.insert(step.position, "failed".to_owned());
                let _ = legacy.update_plan_step_status(
                    plan.id,
                    step.position,
                    "failed",
                    Some(&summary),
                    None,
                );
                let _ = sink
                    .emit(CanonicalEvent::PlanStepCompleted(protocol_plan_step(
                        &plan,
                        step,
                        "failed",
                        Some(&summary),
                    )))
                    .await;
                failed = true;
                break;
            }
            statuses.insert(step.position, "completed".to_owned());
            let _ = legacy.update_plan_step_status(
                plan.id,
                step.position,
                "completed",
                Some(&summary),
                None,
            );
            completed += 1;
            history.push(Message::text(
                MessageRole::Assistant,
                format!("[Step: {}]\n{summary}", step.title),
            ));
            let _ = sink
                .emit(CanonicalEvent::PlanStepCompleted(protocol_plan_step(
                    &plan,
                    step,
                    "completed",
                    Some(&summary),
                )))
                .await;
            let _ = sink
                .emit(CanonicalEvent::PlanProgress(PlanProgress {
                    plan_id: plan.id.to_string(),
                    completed_steps: completed,
                    total_steps: total,
                    message: Some(format!("current step {}", step.position)),
                    status: PlanProgressStatus::Executing,
                }))
                .await;
        }
        let final_status = if cancelled {
            "cancelled"
        } else if failed {
            "failed"
        } else {
            "completed"
        };
        let _ = self
            .finish_plan(&sink, legacy, &actor, &plan, final_status, completed, total)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_plan(
        &self,
        sink: &AppServerEventSink,
        legacy: &LegacyStore,
        actor: &ActorRef,
        plan: &cool_store::domains::plans::Plan,
        status: &str,
        completed: u32,
        total: u32,
    ) -> Result<(), ProtocolError> {
        // A concurrent `plans.cancel` wins: do not clobber a terminal plan.
        let stored = legacy
            .get_plan(&actor.id, plan.conversation_id, plan.id)
            .ok();
        let effective = match stored.as_ref().map(|plan| plan.status.as_str()) {
            Some("cancelled") => "cancelled",
            Some("completed") => "completed",
            Some("failed") => "failed",
            _ => status,
        };
        let already_terminal = stored.as_ref().is_some_and(|plan| {
            matches!(plan.status.as_str(), "cancelled" | "completed" | "failed")
        });
        if !already_terminal {
            let _ = legacy.set_plan_status(&actor.id, plan.conversation_id, plan.id, effective);
        }
        match effective {
            "completed" => {
                let _ = sink
                    .emit(CanonicalEvent::PlanProgress(PlanProgress {
                        plan_id: plan.id.to_string(),
                        completed_steps: completed,
                        total_steps: total,
                        message: None,
                        status: PlanProgressStatus::Completed,
                    }))
                    .await;
                let _ = sink
                    .emit(CanonicalEvent::RunCompleted(RunTerminal {
                        reason: "plan_completed".to_owned(),
                        error_code: None,
                    }))
                    .await;
            }
            "failed" => {
                let _ = sink
                    .emit(CanonicalEvent::PlanProgress(PlanProgress {
                        plan_id: plan.id.to_string(),
                        completed_steps: completed,
                        total_steps: total,
                        message: None,
                        status: PlanProgressStatus::Failed,
                    }))
                    .await;
                let _ = sink
                    .emit(CanonicalEvent::RunFailed(RunTerminal {
                        reason: "plan_failed".to_owned(),
                        error_code: None,
                    }))
                    .await;
            }
            _ => {
                let _ = sink
                    .emit(CanonicalEvent::RunCancelled(RunTerminal {
                        reason: "plan_cancelled".to_owned(),
                        error_code: None,
                    }))
                    .await;
            }
        }
        Ok(())
    }

    /// Expand `ContentPart`s into the text-only user input the Rust runtime
    /// accepts (Python `build_multimodal_content`, `backend/app/multimodal.py`):
    /// extracted text is inlined as `[Attachment: name]`, supported images get
    /// a marker pointing at `image_analyze` (the Rust `Message` has no vision
    /// parts — tracked as an M12 checkpoint gap), opaque files get the
    /// `no text extracted` marker. Ownership matches Python: when the session
    /// is linked to a conversation, artifacts must belong to it.
    fn expand_prompt_parts(
        &self,
        actor: &ActorRef,
        session_id: &str,
        parts: &[ContentPart],
    ) -> Result<String, ProtocolError> {
        if parts
            .iter()
            .all(|part| matches!(part, ContentPart::Text { .. }))
        {
            return Ok(parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"));
        }
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return Err(error(-32010, "legacy_store_unavailable", false));
        };
        let mut seen = HashSet::new();
        let mut artifact_ids = Vec::new();
        for part in parts {
            let raw = match part {
                ContentPart::Artifact { artifact_id } => artifact_id,
                ContentPart::Image { artifact_id, .. } => artifact_id,
                ContentPart::Text { .. } => continue,
            };
            let id = raw
                .parse::<i64>()
                .map_err(|_| legacy::invalid_input(format!("invalid artifact id '{raw}'")))?;
            if seen.insert(id) {
                artifact_ids.push(id);
            }
        }
        if artifact_ids.len() > 10 {
            return Err(legacy::invalid_input(
                "At most 10 artifacts may be attached to one message",
            ));
        }
        let conversation_id = self
            .inner
            .store
            .conversation_id_for_session(&actor.id, session_id)
            .map_err(store_error)?;
        let mut out = String::new();
        let mut emitted = HashSet::new();
        for part in parts {
            match part {
                ContentPart::Text { text } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
                ContentPart::Artifact { artifact_id } | ContentPart::Image { artifact_id, .. } => {
                    let id = artifact_id.parse::<i64>().map_err(|_| {
                        legacy::invalid_input(format!("invalid artifact id '{artifact_id}'"))
                    })?;
                    if !emitted.insert(id) {
                        continue;
                    }
                    let artifact = legacy.get_artifact(&actor.id, id).map_err(|_| {
                        legacy::invalid_input(format!(
                            "Artifact {id} not found in this conversation"
                        ))
                    })?;
                    if let Some(conversation_id) = conversation_id
                        && artifact.conversation_id != conversation_id
                    {
                        return Err(legacy::invalid_input(format!(
                            "Artifact {id} not found in this conversation"
                        )));
                    }
                    if artifact.kind == "image"
                        && blobs::SUPPORTED_IMAGE_TYPES.contains(&artifact.media_type.as_str())
                    {
                        // Rust messages are text-only and there is no vision
                        // tool; the attachment is acknowledged but its pixels
                        // cannot be inspected (parity gap, tracked in M12).
                        out.push_str(&format!(
                            "\n[Attached image: {} — artifact #{}; image content is not visible to this runtime]",
                            artifact.filename, artifact.id
                        ));
                    } else if let Some(text) = artifact.extracted_text.as_deref() {
                        out.push_str(&format!("\n[Attachment: {}]\n{text}", artifact.filename));
                    } else {
                        out.push_str(&format!(
                            "\n[Attached file: {}; no text extracted]",
                            artifact.filename
                        ));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Kick off a canonical run for a research row created by `research.create`
    /// / `research.rerun`. Returns the canonical run id the client streams via
    /// `run.events` / `run.subscribe`. Idempotent: a replay of the same
    /// `research-exec:{id}` key returns the original run without respawning.
    /// `Ok(None)` means no executor is configured (store-only deployment).
    async fn start_research_execution(
        &self,
        research_run_id: i64,
        conversation_id: i64,
        outbound: &Outbound,
        connection: &Arc<Mutex<ConnectionState>>,
    ) -> Result<Option<String>, ProtocolError> {
        let Some(executor) = self.inner.research_executor.as_ref() else {
            return Ok(None);
        };
        let actor = local_actor();
        let session_id = self.ensure_conversation_session(conversation_id)?;
        // Auxiliary, not the session's active run: a rerun on the same
        // conversation must not be gated by `session_run_active`, matching
        // Python where the pipeline is a background task, not a turn.
        let key = format!("research-exec:{research_run_id}");
        if let Some(existing) = self
            .inner
            .store
            .lookup_idempotent::<String>(&actor.id, "research.start", &key, &key)
            .map_err(store_error)?
        {
            return Ok(Some(existing));
        }
        let run_id = self
            .inner
            .store
            .start_auxiliary_run(&actor.id, &session_id)
            .map_err(store_error)?;
        self.inner
            .store
            .record_idempotent(&actor.id, "research.start", &key, &key, &run_id)
            .map_err(store_error)?;
        let (cancel, receiver) = watch::channel(None);
        executor.register(research_run_id, cancel.clone());
        self.inner.state.lock().await.runs.insert(
            run_id.clone(),
            RunRecord {
                cancel,
                terminal: false,
            },
        );
        let connection_id = connection.lock().await.id.clone();
        self.inner
            .run_owners
            .lock()
            .await
            .insert(run_id.clone(), connection_id);
        let server = self.clone();
        let outbound = outbound.clone();
        let spawned = run_id.clone();
        tokio::spawn(async move {
            server
                .run_research(research_run_id, spawned.clone(), outbound, receiver)
                .await;
            if let Some(record) = server.inner.state.lock().await.runs.get_mut(&spawned) {
                record.terminal = true;
            }
        });
        Ok(Some(run_id))
    }

    /// Run the research pipeline under the canonical run: emits `run.started`,
    /// `research.*`, and the matching `run.*` terminal event on the run's
    /// canonical stream (`run.subscribe`/`run.events`) and the owner's outbound.
    async fn run_research(
        &self,
        research_run_id: i64,
        run_id: String,
        outbound: Outbound,
        cancel_rx: watch::Receiver<Option<String>>,
    ) {
        let Some(executor) = self.inner.research_executor.clone() else {
            return;
        };
        let sink = AppServerEventSink {
            server: self.clone(),
            run_id,
            outbound,
            steer_cursor: Arc::new(AtomicU64::new(0)),
            own_user_items: Arc::new(Mutex::new(HashSet::new())),
        };
        let _ = executor.execute(research_run_id, &sink, cancel_rx).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_plan_step(
        &self,
        plan: &cool_store::domains::plans::Plan,
        step: &cool_store::domains::plans::PlanStep,
        history: &[Message],
        workspace: &Workspace,
        model: &str,
        cancel_rx: &watch::Receiver<Option<String>>,
    ) -> (String, bool) {
        let prompt = plan_step_prompt(step);
        match step
            .delegate_role
            .as_deref()
            .filter(|role| !role.is_empty())
        {
            Some(role_name) => {
                self.execute_plan_step_via_subagent(plan, step, &prompt, role_name, cancel_rx)
                    .await
            }
            None => {
                self.execute_plan_step_direct(prompt, history, workspace, model, cancel_rx)
                    .await
            }
        }
    }

    async fn execute_plan_step_direct(
        &self,
        prompt: String,
        history: &[Message],
        workspace: &Workspace,
        model: &str,
        cancel_rx: &watch::Receiver<Option<String>>,
    ) -> (String, bool) {
        let request = AgentRequest {
            model: model.to_owned(),
            history: history.to_vec(),
            user_input: prompt,
            system_prompt: None,
            mode: Some("plan_step".to_owned()),
            temperature: 0.0,
            max_tokens: None,
            limits: AgentLimits {
                max_iterations: 5,
                ..AgentLimits::default()
            },
            tool_names: None,
            tool_context: ToolContext::new(workspace.clone(), self.inner.policy.clone())
                .with_actor(local_actor().id),
        };
        let sink = PlanStepSink::default();
        let outcome = self
            .inner
            .runtime
            .run(
                request,
                &sink,
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
                CancelSignal::from_receiver(cancel_rx.clone()),
            )
            .await;
        match outcome {
            Ok(RunOutcome::Completed { .. }) => (truncate_chars(&sink.text(), 500), false),
            Ok(RunOutcome::Cancelled { .. }) => ("Failed: cancelled".to_owned(), true),
            Ok(RunOutcome::Failed { code, .. }) => (format!("Failed: {code}"), true),
            Err(error) => (format!("Failed: {error}"), true),
        }
    }

    async fn execute_plan_step_via_subagent(
        &self,
        plan: &cool_store::domains::plans::Plan,
        step: &cool_store::domains::plans::PlanStep,
        prompt: &str,
        role_name: &str,
        cancel_rx: &watch::Receiver<Option<String>>,
    ) -> (String, bool) {
        let actor = local_actor();
        let Some(legacy) = self.inner.config.legacy_store.as_deref() else {
            return ("Failed: legacy store unavailable".to_owned(), true);
        };
        let Some(executor) = self.inner.subagent_executor.as_ref() else {
            return ("Failed: subagent executor unavailable".to_owned(), true);
        };
        let role = match legacy.list_subagent_roles() {
            Ok(roles) => roles.into_iter().find(|role| role.name == role_name),
            Err(error) => return (format!("Failed: {error}"), true),
        };
        let Some(role) = role else {
            return (
                format!("Failed: subagent role '{role_name}' not found"),
                true,
            );
        };
        let spec = SubagentLaunchSpec {
            parent_conversation_id: plan.conversation_id,
            role_id: Some(role.id),
            profile_id: None,
            parent_run_id: None,
            research_run_id: None,
            name: Some(format!("plan-step-{}:{role_name}", step.position)),
            prompt: prompt.to_owned(),
            model: None,
        };
        let key = format!("plan-step:{}:{}", plan.id, step.position);
        let run = match executor.launch(&actor.id, spec, &key, &key).await {
            Ok(run) => run,
            Err(error) => return (format!("Failed: {error}"), true),
        };
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            // `plans.cancel` does not signal the run's cancel channel, so also
            // observe the plan status here; otherwise the child could keep
            // running until the hard timeout after the plan was cancelled.
            if cancel_rx.borrow().is_some() || plan_is_cancelled(legacy, &actor, plan) {
                let _ = executor
                    .cancel(&actor.id, run.id, &format!("{key}:cancel"), &key)
                    .await;
                return ("Failed: cancelled".to_owned(), true);
            }
            match legacy.get_subagent_run(&actor.id, run.id) {
                Ok(current) if current.status == "completed" => {
                    return (
                        current
                            .result_summary
                            .clone()
                            .unwrap_or_else(|| "Completed via subagent".to_owned()),
                        false,
                    );
                }
                Ok(current) if ["failed", "cancelled"].contains(&current.status.as_str()) => {
                    return (
                        format!(
                            "Failed: {}",
                            current.error.clone().unwrap_or(current.status)
                        ),
                        true,
                    );
                }
                Ok(_) => {}
                Err(error) => return (format!("Failed: {error}"), true),
            }
            if Instant::now() >= deadline {
                // Stop the child too, so it cannot keep executing after the
                // plan and its run are terminal.
                let _ = executor
                    .cancel(&actor.id, run.id, &format!("{key}:timeout"), &key)
                    .await;
                return ("Failed: subagent timed out".to_owned(), true);
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn finish_cancelled(
        &self,
        run_id: &str,
        reason: &str,
        outbound: Option<&Outbound>,
    ) -> bool {
        let key = format!("internal-cancel:{run_id}");
        let fingerprint = fingerprint(&(run_id, reason, "cool-app-server-internal-cancel"));
        let acceptance = self.inner.store.accept_cancel(
            &local_actor().id,
            &key,
            &fingerprint,
            run_id,
            reason,
            EventProvenance {
                actor: runtime_actor(),
                source: "cool-app-server-m7".to_owned(),
            },
        );
        let terminal = match acceptance {
            Ok(acceptance) => {
                for event in acceptance.events {
                    if let Some(outbound) = outbound {
                        let _ = self.send(outbound, notification(event.clone())).await;
                    }
                    // Subscribers must observe the terminal even when the owner
                    // disconnected before it was recorded.
                    self.publish_to_subscribers(&event).await;
                }
                true
            }
            Err(_) => self
                .inner
                .store
                .run(run_id, &local_actor().id)
                .is_ok_and(|run| run.status.is_terminal()),
        };
        if terminal && let Some(record) = self.inner.state.lock().await.runs.get_mut(run_id) {
            record.terminal = true;
        }
        terminal
    }

    async fn append_event(
        &self,
        run_id: &str,
        event: CanonicalEvent,
        terminal: bool,
    ) -> Option<EventEnvelope> {
        let durable_run = self.inner.store.run(run_id, &local_actor().id).ok()?;
        if durable_run.status.is_terminal() {
            return None;
        }
        let actor = match &event {
            CanonicalEvent::RunCancelled(terminal) if terminal.reason != "disconnect" => {
                local_actor()
            }
            _ => runtime_actor(),
        };
        let envelope = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: durable_run.session_id.clone(),
            run_id: run_id.to_owned(),
            item_id: None,
            seq: 0,
            occurred_at: rfc3339_now(),
            actor,
            source: "cool-app-server-m7".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: BTreeMap::new(),
        };
        let envelope = self
            .inner
            .store
            .append_event_auto(&local_actor().id, envelope)
            .ok()?;
        if let Some(run) = self.inner.state.lock().await.runs.get_mut(run_id) {
            run.terminal = terminal;
        }
        if terminal && let Ok(mut planned) = self.inner.planned_runs.lock() {
            planned.remove(run_id);
        }
        Some(envelope)
    }

    async fn signal_cancel(&self, run_id: &str, reason: &str) -> bool {
        let state = self.inner.state.lock().await;
        let Some(run) = state.runs.get(run_id) else {
            return false;
        };
        if run.terminal {
            return false;
        }
        if run.cancel.borrow().is_some() {
            return true;
        }
        run.cancel.send(Some(reason.to_owned())).is_ok()
    }

    async fn cancel_run(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: String,
        run_id: &str,
        reason: &str,
    ) -> Result<CancelAcceptance, ProtocolError> {
        let reason = mask_secrets(reason);
        let outcome = self
            .inner
            .store
            .accept_cancel(
                actor_id,
                key,
                &fingerprint,
                run_id,
                &reason,
                EventProvenance {
                    actor: local_actor(),
                    source: "cool-app-server-m7".to_owned(),
                },
            )
            .map_err(store_error)?;
        if outcome.created {
            let _ = self.signal_cancel(run_id, &reason).await;
        }
        Ok(outcome)
    }

    async fn event_page(
        &self,
        response_id: &RpcId,
        run_id: &str,
        after_seq: Option<u64>,
        limit: u16,
    ) -> Result<EventPage, ProtocolError> {
        if limit == 0 || limit > self.inner.config.event_page_limit {
            return Err(error(-32602, "invalid_event_page_limit", false));
        }
        let eligible = self
            .inner
            .store
            .events(run_id, &local_actor().id, after_seq, usize::from(limit) + 1)
            .map_err(store_error)?;
        let mut events = Vec::new();
        for event in eligible.iter().take(limit as usize) {
            let mut candidate = events.clone();
            candidate.push(event.clone());
            let candidate_page = EventPage {
                events: candidate.clone(),
                next_cursor: candidate.last().map(|event| EventCursor {
                    run_id: run_id.to_owned(),
                    after_seq: Some(event.seq),
                }),
                has_more: candidate.len() < eligible.len(),
            };
            let fits = serde_json::to_vec(&success(
                response_id.clone(),
                ResponsePayload::EventPage(candidate_page),
            ))
            .is_ok_and(|encoded| encoded.len() <= self.inner.config.max_frame_bytes);
            if !fits {
                break;
            }
            events = candidate;
        }
        if events.is_empty() && !eligible.is_empty() {
            return Err(error(-32008, "outbound_frame_too_large", false));
        }
        let has_more = events.len() < eligible.len();
        let next_cursor = events.last().map(|event| EventCursor {
            run_id: run_id.to_owned(),
            after_seq: Some(event.seq),
        });
        Ok(EventPage {
            events,
            next_cursor,
            has_more,
        })
    }
}

#[derive(Clone)]
struct AppServerEventSink {
    server: AppServer,
    run_id: String,
    outbound: Outbound,
    steer_cursor: Arc<AtomicU64>,
    own_user_items: Arc<Mutex<HashSet<String>>>,
}

struct LifecycleEventSink {
    inner: AppServerEventSink,
    lifecycle: Arc<dyn RunLifecycle>,
    policy: CapabilityPolicy,
    prompt: String,
}

#[async_trait]
impl EventSink for LifecycleEventSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        let payload = lifecycle_payload(&event);
        match &event {
            CanonicalEvent::RunStarted(_) => {
                let envelope = self.inner.emit(event).await?;
                self.dispatch("SessionStart", payload.clone()).await;
                self.dispatch(
                    "UserPromptSubmit",
                    serde_json::json!({"content": self.prompt}),
                )
                .await;
                Ok(envelope)
            }
            CanonicalEvent::ToolStarted(_) => {
                self.dispatch("PreToolUse", payload).await;
                self.inner.emit(event).await
            }
            CanonicalEvent::ToolApprovalRequired(_) => {
                self.dispatch("PermissionRequest", payload).await;
                self.inner.emit(event).await
            }
            CanonicalEvent::SessionCompacted(_) => {
                let envelope = self.inner.emit(event).await?;
                self.dispatch("PostCompact", payload).await;
                Ok(envelope)
            }
            CanonicalEvent::SubagentStarted(_) => {
                self.dispatch("SubagentStart", payload).await;
                self.inner.emit(event).await
            }
            CanonicalEvent::ToolCompleted(_) | CanonicalEvent::ToolFailed(_) => {
                let envelope = self.inner.emit(event).await?;
                self.dispatch("PostToolUse", payload).await;
                Ok(envelope)
            }
            CanonicalEvent::SubagentCompleted(_) | CanonicalEvent::SubagentFailed(_) => {
                let envelope = self.inner.emit(event).await?;
                self.dispatch("SubagentStop", payload).await;
                Ok(envelope)
            }
            CanonicalEvent::RunCompleted(_)
            | CanonicalEvent::RunFailed(_)
            | CanonicalEvent::RunCancelled(_) => {
                if self.terminal_already_recorded() {
                    return self.inner.emit(event).await;
                }
                let cancelled = matches!(event, CanonicalEvent::RunCancelled(_));
                let envelope = self.inner.emit(event).await?;
                let hook = if cancelled { "Interrupt" } else { "Stop" };
                self.dispatch(hook, payload.clone()).await;
                self.dispatch("SessionEnd", payload).await;
                Ok(envelope)
            }
            _ => self.inner.emit(event).await,
        }
    }

    async fn load_history(&self) -> Result<Vec<Message>, RuntimeError> {
        self.inner.load_history().await
    }

    async fn before_compaction(&self, history: &[Message]) -> Result<(), RuntimeError> {
        self.dispatch(
            "PreCompact",
            serde_json::json!({"historyItems": history.len()}),
        )
        .await;
        Ok(())
    }

    async fn reserve_usage(&self, usage: &Usage) -> Result<(), RuntimeError> {
        self.inner.reserve_usage(usage).await
    }

    async fn drain_steers(&self) -> Result<Vec<Message>, RuntimeError> {
        self.inner.drain_steers().await
    }
}

impl LifecycleEventSink {
    async fn dispatch(&self, event: &str, payload: serde_json::Value) {
        let events = self.lifecycle.on_event(event, payload, &self.policy).await;
        for lifecycle_event in events {
            let _ = self.inner.emit(lifecycle_event).await;
        }
    }

    fn terminal_already_recorded(&self) -> bool {
        self.inner
            .server
            .inner
            .store
            .all_events(&self.inner.run_id, &local_actor().id)
            .is_ok_and(|events| {
                events.iter().any(|event| {
                    matches!(
                        event.event,
                        CanonicalEvent::RunCompleted(_)
                            | CanonicalEvent::RunFailed(_)
                            | CanonicalEvent::RunCancelled(_)
                    )
                })
            })
    }
}

fn lifecycle_payload(event: &CanonicalEvent) -> serde_json::Value {
    match event {
        CanonicalEvent::ToolStarted(payload) => {
            serde_json::json!({"tool": payload.name, "callId": payload.call_id})
        }
        CanonicalEvent::ToolCompleted(payload) => {
            serde_json::json!({"tool": payload.name, "callId": payload.call_id})
        }
        CanonicalEvent::ToolApprovalRequired(payload) => serde_json::json!({
            "tool": payload.name,
            "callId": payload.call_id,
            "reason": mask_secrets(&payload.reason),
            "approvalId": payload.approval_id,
        }),
        CanonicalEvent::ToolFailed(payload) => serde_json::json!({
            "tool": payload.name,
            "callId": payload.call_id,
            "errorCode": payload.error_code,
        }),
        CanonicalEvent::SubagentStarted(payload)
        | CanonicalEvent::SubagentCompleted(payload)
        | CanonicalEvent::SubagentFailed(payload) => serde_json::json!({
            "subagentRunId": payload.subagent_run_id,
            "name": payload.name,
            "status": payload.status,
        }),
        CanonicalEvent::SessionCompacted(payload) => serde_json::json!({
            "retainedItems": payload.retained_items,
            "summaryItemId": payload.summary_item_id,
        }),
        CanonicalEvent::RunCompleted(payload)
        | CanonicalEvent::RunFailed(payload)
        | CanonicalEvent::RunCancelled(payload) => serde_json::json!({
            "reason": mask_secrets(&payload.reason),
            "errorCode": payload.error_code,
        }),
        _ => {
            let masked = mask_canonical_event(event.clone()).unwrap_or_else(|_| event.clone());
            serde_json::to_value(masked)
                .unwrap_or_else(|_| serde_json::json!({"kind":"unserializable"}))
        }
    }
}

#[async_trait]
impl EventSink for AppServerEventSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        let envelope = self.emit_once(event).await?;
        if matches!(
            &envelope.event,
            CanonicalEvent::ItemCompleted(item) if item.role.as_deref() == Some("user")
        ) {
            self.own_user_items
                .lock()
                .await
                .insert(envelope.event_id.clone());
        }
        Ok(envelope)
    }

    async fn load_history(&self) -> Result<Vec<Message>, RuntimeError> {
        let run = self
            .server
            .inner
            .store
            .run(&self.run_id, &local_actor().id)?;
        let events = self
            .server
            .inner
            .store
            .session_events(&run.session_id, &local_actor().id)?;
        let history = history_from_events(&events)?;
        if let Some(last_seq) = events
            .iter()
            .filter(|event| event.run_id == self.run_id)
            .map(|event| event.seq)
            .max()
        {
            self.steer_cursor.fetch_max(last_seq, Ordering::SeqCst);
        }
        Ok(history)
    }

    async fn reserve_usage(&self, usage: &Usage) -> Result<(), RuntimeError> {
        self.server.inner.store.reserve_budget(
            &local_actor().id,
            &format!("run:{}", self.run_id),
            BudgetDelta {
                tokens: usage.total_tokens,
                cost_microusd: usage.cost_micro_usd,
                iterations: 1,
                proactive_actions: 0,
            },
        )?;
        Ok(())
    }

    async fn drain_steers(&self) -> Result<Vec<Message>, RuntimeError> {
        let after = self.steer_cursor.load(Ordering::SeqCst);
        let events = self.server.inner.store.events(
            &self.run_id,
            &local_actor().id,
            Some(after),
            usize::MAX,
        )?;
        let mut messages = Vec::new();
        let mut max_seq = after;
        let mut own = self.own_user_items.lock().await;
        for envelope in &events {
            max_seq = max_seq.max(envelope.seq);
            if let CanonicalEvent::ItemCompleted(item) = &envelope.event
                && item.role.as_deref() == Some("user")
            {
                if own.remove(&envelope.event_id) {
                    // The loop emitted this user item itself; it is already in
                    // history and must not be delivered twice.
                    continue;
                }
                if let Some(content) = item.content.clone() {
                    messages.push(Message::text(MessageRole::User, content));
                }
            }
        }
        drop(own);
        self.steer_cursor.fetch_max(max_seq, Ordering::SeqCst);
        Ok(messages)
    }
}

impl AppServerEventSink {
    async fn emit_once(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        let mut event = mask_canonical_event(event)?;
        self.persist_plan_created(&mut event);
        if matches!(event, CanonicalEvent::RunCancelled(_)) {
            let run = self
                .server
                .inner
                .store
                .run(&self.run_id, &local_actor().id)?;
            if run.status == cool_state::RunStatus::Cancelled
                && let Some(existing) = self
                    .server
                    .inner
                    .store
                    .all_events(&self.run_id, &local_actor().id)?
                    .into_iter()
                    .last()
                    .filter(|event| matches!(event.event, CanonicalEvent::RunCancelled(_)))
            {
                return Ok(existing);
            }
        }
        if let CanonicalEvent::ToolFailed(requested) = &event {
            let run = self
                .server
                .inner
                .store
                .run(&self.run_id, &local_actor().id)?;
            if run.status.is_terminal()
                && let Some(existing) = self
                    .server
                    .inner
                    .store
                    .all_events(&self.run_id, &local_actor().id)?
                    .into_iter()
                    .find(|event| {
                        matches!(
                            &event.event,
                            CanonicalEvent::ToolFailed(stored)
                                if stored.call_id == requested.call_id
                        )
                    })
            {
                return Ok(existing);
            }
        }
        let run = self
            .server
            .inner
            .store
            .run(&self.run_id, &local_actor().id)?;
        if !self
            .server
            .preview_event_frame_fits(&run.session_id, event.clone())
        {
            return Err(RuntimeError::Sink(
                "event exceeds the negotiated transport frame limit".to_owned(),
            ));
        }
        let terminal = matches!(
            event,
            CanonicalEvent::RunCompleted(_)
                | CanonicalEvent::RunFailed(_)
                | CanonicalEvent::RunCancelled(_)
        );
        let envelope = self
            .server
            .append_event(&self.run_id, event, terminal)
            .await
            .ok_or_else(|| RuntimeError::Sink("run no longer accepts events".to_owned()))?;
        // Fan out to subscribers before the owner send: the event is already
        // durable, and a failed owner delivery must not withhold the terminal
        // from `run.subscribe` connections.
        self.server.publish_to_subscribers(&envelope).await;
        if !self
            .server
            .send(&self.outbound, notification(envelope.clone()))
            .await
        {
            return Err(RuntimeError::Sink(
                "client disconnected while publishing run event".to_owned(),
            ));
        }
        Ok(envelope)
    }

    /// Persist a durable draft plan for a `plan.created` event whose run is
    /// bound to a legacy conversation, stamping the store id so the client can
    /// approve/execute it through the App Protocol. Best-effort: a run with no
    /// conversation link leaves `store_plan_id` as `None`.
    fn persist_plan_created(&self, event: &mut CanonicalEvent) {
        let CanonicalEvent::PlanCreated(plan) = event else {
            return;
        };
        if plan.store_plan_id.is_some() {
            return;
        }
        // One durable draft per run: a second `update_plan` call reuses the id.
        if let Ok(known) = self.server.inner.planned_runs.lock()
            && let Some(id) = known.get(&self.run_id)
        {
            plan.store_plan_id = Some(*id);
            return;
        }
        let Some(legacy) = self.server.inner.config.legacy_store.as_deref() else {
            return;
        };
        let actor = local_actor();
        let Ok(run) = self.server.inner.store.run(&self.run_id, &actor.id) else {
            return;
        };
        let Ok(Some(conversation_id)) = self
            .server
            .inner
            .store
            .conversation_id_for_session(&actor.id, &run.session_id)
        else {
            return;
        };
        let steps = serde_json::Value::Array(
            plan.steps
                .iter()
                .map(|step| {
                    serde_json::json!({
                        "position": step.position,
                        "title": step.title,
                    })
                })
                .collect(),
        );
        let Ok(created) = legacy.create_plan(
            &actor.id,
            conversation_id,
            None,
            plan.title.as_deref(),
            &steps,
        ) else {
            return;
        };
        plan.store_plan_id = Some(created.id);
        if let Ok(mut known) = self.server.inner.planned_runs.lock() {
            known.insert(self.run_id.clone(), created.id);
        }
    }
}

struct AppServerApprovalGate {
    server: AppServer,
    run_id: String,
    session_id: String,
    outbound: Outbound,
}

#[async_trait]
impl ApprovalGate for AppServerApprovalGate {
    async fn request(
        &self,
        request: ApprovalRequest,
        _sink: &dyn EventSink,
        cancel: &mut CancelSignal,
    ) -> Result<cool_protocol::ApprovalOutcome, RuntimeError> {
        let masked = mask_canonical_event(CanonicalEvent::ToolApprovalRequired(
            cool_protocol::ToolApprovalRequired {
                call_id: request.call.call_id.clone(),
                name: request.call.name.clone(),
                arguments: request.call.arguments.clone().into_iter().collect(),
                reason: request.reason.clone(),
                approval_id: request.approval_id.clone(),
                revision: 1,
                breakpoint_type: None,
                result_preview: None,
                current_content: None,
            },
        ))?;
        let CanonicalEvent::ToolApprovalRequired(masked) = masked else {
            unreachable!("event variant is preserved by masking")
        };
        if !self.server.preview_event_frame_fits(
            &self.session_id,
            CanonicalEvent::ToolApprovalRequired(masked.clone()),
        ) {
            return Err(RuntimeError::Sink(
                "approval event exceeds the negotiated transport frame limit".to_owned(),
            ));
        }
        let ticket = self.server.inner.store.create_approval_with_arguments(
            &local_actor().id,
            &self.session_id,
            &self.run_id,
            &request.call.call_id,
            &request.call.name,
            &masked.arguments,
            &masked.reason,
        )?;
        let (sender, mut receiver) = watch::channel(None);
        self.server
            .inner
            .approval_waiters
            .lock()
            .await
            .insert(ticket.approval_id.clone(), sender);
        if ticket.created {
            let event = self
                .server
                .inner
                .store
                .all_events(&self.run_id, &local_actor().id)?
                .into_iter()
                .last()
                .ok_or_else(|| RuntimeError::Sink("approval event is missing".to_owned()))?;
            // Subscribers see the approval request even if the owner is gone;
            // the waiter still fails closed below when the owner cannot be
            // reached, so the run does not silently hang.
            self.server.publish_to_subscribers(&event).await;
            if !self.server.send(&self.outbound, notification(event)).await {
                self.server
                    .inner
                    .approval_waiters
                    .lock()
                    .await
                    .remove(&ticket.approval_id);
                return Err(RuntimeError::Sink(
                    "client disconnected before approval delivery".to_owned(),
                ));
            }
        }
        if let Some(outcome) = self
            .server
            .inner
            .store
            .approval_outcome(&local_actor().id, &ticket.approval_id)?
        {
            self.server
                .inner
                .approval_waiters
                .lock()
                .await
                .remove(&ticket.approval_id);
            return Ok(outcome);
        }
        let result = tokio::select! {
            outcome = async {
                loop {
                    receiver.changed().await.map_err(|_| RuntimeError::Sink("approval channel closed".to_owned()))?;
                    if let Some(outcome) = receiver.borrow().clone() {
                        return Ok(outcome);
                    }
                }
            } => outcome,
            reason = cancel.wait() => Err(RuntimeError::Sink(format!("approval cancelled: {reason}"))),
        };
        if result.is_err() {
            self.server
                .inner
                .approval_waiters
                .lock()
                .await
                .remove(&ticket.approval_id);
        }
        result
    }
}

#[cfg(unix)]
struct SocketCleanup(std::path::PathBuf);

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Captures the concatenated content deltas of one plan step; a plan step does
/// not project its own transcript.
#[derive(Default)]
struct PlanStepSink {
    text: std::sync::Mutex<String>,
}

impl PlanStepSink {
    fn text(&self) -> String {
        self.text
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl EventSink for PlanStepSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, RuntimeError> {
        if let CanonicalEvent::ContentDelta(delta) = &event
            && let Ok(mut text) = self.text.lock()
        {
            text.push_str(&delta.text);
        }
        Ok(preview_event_envelope("plan-step", event))
    }
}

/// Kahn topological order of plan steps by `position`; unknown dependencies are
/// ignored and any remaining (cycle) steps append in position order (Python
/// `planning._topological_order`).
fn topological_order(
    steps: &[cool_store::domains::plans::PlanStep],
) -> Vec<cool_store::domains::plans::PlanStep> {
    let by_position = steps
        .iter()
        .map(|step| (step.position, step))
        .collect::<BTreeMap<_, _>>();
    let mut in_degree = steps
        .iter()
        .map(|step| (step.position, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut dependents: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for step in steps {
        for dependency in depends_on(step) {
            if by_position.contains_key(&dependency) {
                *in_degree.entry(step.position).or_default() += 1;
                dependents
                    .entry(dependency)
                    .or_default()
                    .push(step.position);
            }
        }
    }
    let mut queue = in_degree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(position, _)| *position)
        .collect::<Vec<_>>();
    queue.sort();
    let mut order = Vec::with_capacity(steps.len());
    while !queue.is_empty() {
        let position = queue.remove(0);
        if let Some(step) = by_position.get(&position) {
            order.push((*step).clone());
        }
        for dependent in dependents.get(&position).cloned().unwrap_or_default() {
            if let Some(degree) = in_degree.get_mut(&dependent) {
                *degree = degree.saturating_sub(1);
                if *degree == 0 {
                    queue.push(dependent);
                }
            }
        }
        queue.sort();
    }
    let seen = order
        .iter()
        .map(|step| step.position)
        .collect::<BTreeSet<_>>();
    let mut remaining = steps
        .iter()
        .filter(|step| !seen.contains(&step.position))
        .cloned()
        .collect::<Vec<_>>();
    remaining.sort_by_key(|step| step.position);
    order.extend(remaining);
    order
}

fn depends_on(step: &cool_store::domains::plans::PlanStep) -> Vec<i64> {
    step.depends_on
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_i64).collect())
        .unwrap_or_default()
}

/// A step runs when every known dependency is completed or skipped.
fn dependencies_met(
    step: &cool_store::domains::plans::PlanStep,
    statuses: &BTreeMap<i64, String>,
) -> bool {
    depends_on(step).iter().all(|dependency| {
        statuses
            .get(dependency)
            .is_none_or(|status| status == "completed" || status == "skipped")
    })
}

fn protocol_plan_step(
    plan: &cool_store::domains::plans::Plan,
    step: &cool_store::domains::plans::PlanStep,
    status: &str,
    result_summary: Option<&str>,
) -> ProtocolPlanStep {
    ProtocolPlanStep {
        plan_id: plan.id.to_string(),
        position: step.position as u32,
        title: step.title.clone(),
        status: status.to_owned(),
        result_summary: result_summary.map(str::to_owned),
    }
}

/// True when an operator cancelled the plan while it was executing.
fn plan_is_cancelled(
    legacy: &LegacyStore,
    actor: &ActorRef,
    plan: &cool_store::domains::plans::Plan,
) -> bool {
    legacy
        .get_plan(&actor.id, plan.conversation_id, plan.id)
        .is_ok_and(|stored| stored.status == "cancelled")
}

/// Bounded conversation history for plan-step context (user/assistant turns).
fn plan_history(legacy: &LegacyStore, actor_id: &str, conversation_id: i64) -> Vec<Message> {
    // Newest 200 turns, oldest-first: bounded and recent.
    let Ok(window) = legacy.recent_messages(actor_id, conversation_id, 200) else {
        return Vec::new();
    };
    window
        .messages
        .iter()
        .filter_map(|message| match message.role.as_str() {
            "user" => message
                .content
                .clone()
                .map(|content| Message::text(MessageRole::User, content)),
            "assistant" => message
                .content
                .clone()
                .filter(|content| !content.is_empty())
                .map(|content| Message::text(MessageRole::Assistant, content)),
            _ => None,
        })
        .collect()
}

fn plan_step_prompt(step: &cool_store::domains::plans::PlanStep) -> String {
    let mut prompt = format!("Execute this plan step:\n\n**{}**\n", step.title);
    if let Some(description) = step.description.as_deref() {
        prompt.push_str(&format!("\n{description}\n"));
    }
    prompt.push_str("\nComplete this step and provide a brief summary of what was accomplished.");
    prompt
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() > limit {
        text.chars().take(limit).collect()
    } else {
        text.to_owned()
    }
}

fn local_actor() -> ActorRef {
    ActorRef {
        id: "local-user".to_owned(),
        kind: ActorKind::LocalUser,
    }
}

/// The single local operator actor id — blob routes in `cool-http` resolve it
/// here instead of duplicating the literal.
pub fn local_actor_id() -> String {
    local_actor().id
}

fn runtime_actor() -> ActorRef {
    ActorRef {
        id: "cool-app-server".to_owned(),
        kind: ActorKind::System,
    }
}

/// Replay one stored webhook event: record a new authenticated event row and
/// re-dispatch it through the task executor (linked task or ad-hoc one-shot),
/// mirroring `app/webhooks/service.py::replay_event`.
async fn replay_webhook(
    store: &LegacyStore,
    executor: &Arc<TaskExecutor>,
    actor: &ActorRef,
    event_id: i64,
    endpoint_id: i64,
) -> Result<cool_store::domains::webhooks::WebhookEvent, cool_store::StoreError> {
    let event = store.get_webhook_event(&actor.id, event_id)?;
    if event.endpoint_id != endpoint_id {
        return Err(cool_store::StoreError::NotFound("webhook event"));
    }
    let endpoint = store.get_endpoint(&actor.id, endpoint_id)?;
    // A replay is treated as an authenticated arrival, like Python forcing
    // `signature_valid=True`.
    let replay = store.record_webhook_event(
        endpoint_id,
        &NewWebhookEvent {
            event_type: event.event_type.clone(),
            payload: event.payload.clone(),
            signature_valid: true,
            status: Some("processing"),
        },
    )?;
    let task_run_id = if let Some(task_id) = endpoint.task_id {
        match executor.dispatch_task(actor, task_id).await {
            Ok(run) => Some(run.id),
            Err(error) => {
                store.update_webhook_event(replay.id, "failed", Some(&error_text(&error)), None)?;
                return store.get_webhook_event(&actor.id, replay.id);
            }
        }
    } else if let Some(template) = endpoint
        .prompt_template
        .as_deref()
        .filter(|template| !template.is_empty())
    {
        // Python `event.payload or {}`: a null payload becomes an empty object.
        let payload_value = event
            .payload
            .clone()
            .filter(|value| !value.is_null())
            .unwrap_or_else(|| serde_json::json!({}));
        let payload = serde_json::to_string(&payload_value).unwrap_or_default();
        let prompt = template.replace("{event}", &payload.chars().take(4000).collect::<String>());
        let name = format!(
            "[Webhook] {}: {}",
            endpoint.name,
            event.event_type.as_deref().unwrap_or("event")
        );
        match executor.dispatch_adhoc(actor, name, prompt).await {
            Ok(run) => Some(run.id),
            Err(error) => {
                store.update_webhook_event(replay.id, "failed", Some(&error_text(&error)), None)?;
                return store.get_webhook_event(&actor.id, replay.id);
            }
        }
    } else {
        None
    };
    store.update_webhook_event(replay.id, "completed", None, task_run_id)?;
    store.get_webhook_event(&actor.id, replay.id)
}

/// Webhook event error text, capped like Python's `str(exc)[:1000]`.
fn error_text(error: &cool_store::StoreError) -> String {
    error.to_string().chars().take(1000).collect()
}

/// Most recent legacy messages projected into one session import. Older
/// history stays readable through the paginated `conversations.messages`
/// command, so the canonical session keeps a bounded working context.
const MAX_IMPORTED_MESSAGES: usize = 10_000;

/// System directive for `conversations.compact` (rolling summary).
const SUMMARIZER_SYSTEM_PROMPT: &str = "You summarize a conversation for the assistant's future context. Keep durable facts, decisions, open tasks and user preferences; drop pleasantries. Reply with the summary only.";

fn session_conversation_payload(link: ConversationLink) -> ResponsePayload {
    ResponsePayload::SessionForConversation(SessionConversationResult {
        session_id: link.session_id,
        conversation_id: link.conversation_id,
        created: link.created,
        imported_events: link.imported_events,
        truncated: link.truncated,
    })
}

/// Drop leading tool rows so a bounded window never starts with a tool result
/// whose assistant tool call was cut off: providers reject an orphan tool role.
/// Returns how many rows were dropped.
fn trim_orphan_tool_rows(messages: &mut Vec<cool_store::domains::conversations::Message>) -> usize {
    let leading_tools = messages
        .iter()
        .take_while(|message| message.role == "tool")
        .count();
    messages.drain(..leading_tools);
    leading_tools
}

/// Match a legacy tool result row to a pending assistant tool call.
///
/// The stored id wins; otherwise the result falls back to a uniquely named
/// pending call and then to the only pending call. `None` means the row is an
/// orphan result (the assistant call was never persisted or was truncated).
fn match_pending_tool_call(
    pending: &[ToolRequested],
    stored_call_id: Option<&str>,
    name: &str,
) -> Option<usize> {
    if let Some(call_id) = stored_call_id
        && let Some(index) = pending.iter().position(|call| call.call_id == call_id)
    {
        return Some(index);
    }
    let mut named = pending
        .iter()
        .enumerate()
        .filter(|(_, call)| call.name == name);
    match (named.next(), named.next()) {
        (Some((index, _)), None) => Some(index),
        _ if pending.len() == 1 => Some(0),
        _ => None,
    }
}

/// Flush assistant tool calls that never received a persisted result as
/// `tool.failed`, mirroring the Python runtime's `_backfill_missing_tool_results`
/// so imported history stays a valid provider transcript.
fn flush_unanswered_tool_calls(
    events: &mut Vec<ImportedHistoryEvent>,
    pending: &mut Vec<ToolRequested>,
    occurred_at: &str,
) {
    for call in pending.drain(..) {
        events.push(ImportedHistoryEvent {
            occurred_at: occurred_at.to_owned(),
            event: CanonicalEvent::ToolFailed(cool_protocol::ToolFailed {
                call_id: call.call_id,
                name: call.name,
                error_code: "legacy_tool_unanswered".to_owned(),
                message: Some("tool call has no persisted result".to_owned()),
            }),
        });
    }
}

/// Normalize a SQLAlchemy `YYYY-MM-DD HH:MM:SS.ffffff` timestamp to the
/// RFC3339 form the canonical event envelope uses. Unknown shapes pass through.
fn normalize_legacy_timestamp(value: &str) -> String {
    let Some(seconds) = cool_store::time::parse_python_datetime(value) else {
        return value.to_owned();
    };
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date(days);
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

/// Project the legacy transcript into canonical history events.
///
/// Legacy tool rows carry their call id inside `tool_result`; assistant rows
/// carry OpenAI-shaped `tool_calls`. Missing call ids or names degrade to a
/// stable synthetic value instead of dropping the row, and failed tool rows
/// map to `tool.failed` so replayed history keeps their error semantics.
/// Structurally invalid JSON in a legacy column is rejected earlier by the
/// store's strict column parsing, failing the whole read closed.
fn legacy_history_events(
    messages: &[cool_store::domains::conversations::Message],
) -> Vec<ImportedHistoryEvent> {
    let mut events = Vec::new();
    let mut pending: Vec<ToolRequested> = Vec::new();
    let mut last_occurred_at = String::new();
    for message in messages {
        let occurred_at = normalize_legacy_timestamp(&message.created_at);
        last_occurred_at = occurred_at.clone();
        match message.role.as_str() {
            "user" => {
                flush_unanswered_tool_calls(&mut events, &mut pending, &occurred_at);
                if message.content.is_some() {
                    events.push(ImportedHistoryEvent {
                        occurred_at,
                        event: CanonicalEvent::ItemCompleted(ItemEvent {
                            role: Some("user".to_owned()),
                            content: message.content.clone(),
                            tool_calls: Vec::new(),
                        }),
                    });
                }
            }
            "assistant" => {
                flush_unanswered_tool_calls(&mut events, &mut pending, &occurred_at);
                if let Some(thinking) = message
                    .thinking
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                {
                    events.push(ImportedHistoryEvent {
                        occurred_at: occurred_at.clone(),
                        event: CanonicalEvent::ReasoningDelta(TextDelta {
                            text: thinking.to_owned(),
                            channel: Some("analysis".to_owned()),
                        }),
                    });
                }
                let tool_calls = legacy_tool_calls(message);
                if message.content.is_some() || !tool_calls.is_empty() {
                    pending.extend(tool_calls.iter().cloned());
                    events.push(ImportedHistoryEvent {
                        occurred_at,
                        event: CanonicalEvent::ItemCompleted(ItemEvent {
                            role: Some("assistant".to_owned()),
                            content: message.content.clone(),
                            tool_calls,
                        }),
                    });
                }
            }
            "tool" => {
                let tool_result = message.tool_result.as_ref();
                let stored_call_id = tool_result
                    .and_then(|value| value.get("tool_call_id"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
                let name = tool_result
                    .and_then(|value| value.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                let matched = match_pending_tool_call(&pending, stored_call_id.as_deref(), &name);
                let call_id = match matched {
                    Some(index) => pending.remove(index).call_id,
                    None => stored_call_id.unwrap_or_else(|| {
                        format!("legacy-tool-{}-{}", message.conversation_id, message.id)
                    }),
                };
                let result = tool_result
                    .and_then(|value| value.get("result"))
                    .cloned()
                    .or_else(|| message.content.clone().map(serde_json::Value::String))
                    .unwrap_or(serde_json::Value::Null);
                let is_error = result
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let event = if is_error {
                    let message_text = result
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| result.get("output").and_then(serde_json::Value::as_str))
                        .map(str::to_owned);
                    CanonicalEvent::ToolFailed(cool_protocol::ToolFailed {
                        call_id,
                        name,
                        error_code: "legacy_tool_error".to_owned(),
                        message: message_text,
                    })
                } else {
                    CanonicalEvent::ToolCompleted(ToolCompleted {
                        call_id,
                        name,
                        result,
                    })
                };
                events.push(ImportedHistoryEvent { occurred_at, event });
            }
            _ => {
                flush_unanswered_tool_calls(&mut events, &mut pending, &occurred_at);
            }
        }
    }
    flush_unanswered_tool_calls(&mut events, &mut pending, &last_occurred_at);
    events
}

fn legacy_tool_calls(message: &cool_store::domains::conversations::Message) -> Vec<ToolRequested> {
    let Some(calls) = message
        .tool_calls
        .as_ref()
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let name = call
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("unknown")
                .to_owned();
            let call_id = call
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("legacy-{}-{}-{index}", message.conversation_id, message.id)
                });
            let arguments = match call.get("arguments") {
                Some(serde_json::Value::Object(map)) => map.clone().into_iter().collect(),
                None | Some(serde_json::Value::Null) => BTreeMap::new(),
                Some(other) => BTreeMap::from([("value".to_owned(), other.clone())]),
            };
            ToolRequested {
                call_id,
                name,
                arguments,
            }
        })
        .collect()
}

/// One projected history item together with durable event cursors. `cursor`
/// points at the item's own event; `start_cursor` points at the first event of
/// the group (the leading reasoning deltas, when present), so a page boundary
/// never splits reasoning from the assistant item it belongs to.
struct HistoryEntry {
    start_cursor: u64,
    item: HistoryItem,
}

/// Project an ordered event window into history items, attaching accumulated
/// reasoning deltas to the assistant item that follows them.
fn history_entries_from_window(
    window: &[(u64, EventEnvelope)],
    run_models: &std::collections::HashMap<String, String>,
) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    let mut reasoning = String::new();
    let mut group_start: Option<u64> = None;
    // Per-run model resolved by the caller (a page can start mid-run) plus any
    // `run.started` seen in the window, and the latest usage seen since the
    // previous assistant item, so a projected assistant item can carry both.
    let mut models = run_models.clone();
    let mut pending_usage: Option<UsageUpdated> = None;
    for (cursor, envelope) in window {
        match &envelope.event {
            CanonicalEvent::RunStarted(started) => {
                if let Some(model) = started.model.as_ref() {
                    models.insert(envelope.run_id.clone(), model.clone());
                }
                // Usage never crosses a run boundary.
                pending_usage = None;
            }
            CanonicalEvent::RunCompleted(_)
            | CanonicalEvent::RunFailed(_)
            | CanonicalEvent::RunCancelled(_) => {
                pending_usage = None;
            }
            CanonicalEvent::UsageUpdated(usage) => {
                pending_usage = Some(usage.clone());
            }
            CanonicalEvent::ReasoningDelta(delta) => {
                if reasoning.is_empty() {
                    group_start = Some(*cursor);
                }
                reasoning.push_str(&delta.text);
            }
            CanonicalEvent::ItemCompleted(item) => {
                let Some(role) = item.role.as_deref() else {
                    continue;
                };
                if !matches!(role, "user" | "assistant") {
                    continue;
                }
                let attached = if role == "assistant" && !reasoning.is_empty() {
                    Some(std::mem::take(&mut reasoning))
                } else {
                    reasoning.clear();
                    None
                };
                let (usage, model) = if role == "assistant" {
                    (pending_usage.take(), models.get(&envelope.run_id).cloned())
                } else {
                    pending_usage = None;
                    (None, None)
                };
                let start_cursor = group_start.take().unwrap_or(*cursor);
                entries.push(HistoryEntry {
                    start_cursor,
                    item: HistoryItem {
                        cursor: *cursor,
                        occurred_at: envelope.occurred_at.clone(),
                        run_id: envelope.run_id.clone(),
                        role: role.to_owned(),
                        content: item.content.clone(),
                        reasoning: attached,
                        tool_calls: item.tool_calls.clone(),
                        tool_call_id: None,
                        name: None,
                        model,
                        usage,
                        compact_up_to_cursor: None,
                    },
                });
            }
            CanonicalEvent::ToolCompleted(tool) => {
                reasoning.clear();
                group_start = None;
                pending_usage = None;
                entries.push(HistoryEntry {
                    start_cursor: *cursor,
                    item: HistoryItem {
                        cursor: *cursor,
                        occurred_at: envelope.occurred_at.clone(),
                        run_id: envelope.run_id.clone(),
                        role: "tool".to_owned(),
                        content: Some(
                            serde_json::to_string(&tool.result)
                                .unwrap_or_else(|_| "null".to_owned()),
                        ),
                        reasoning: None,
                        tool_calls: Vec::new(),
                        tool_call_id: Some(tool.call_id.clone()),
                        name: Some(tool.name.clone()),
                        model: None,
                        usage: None,
                        compact_up_to_cursor: None,
                    },
                });
            }
            CanonicalEvent::ToolFailed(tool) => {
                reasoning.clear();
                group_start = None;
                pending_usage = None;
                entries.push(HistoryEntry {
                    start_cursor: *cursor,
                    item: HistoryItem {
                        cursor: *cursor,
                        occurred_at: envelope.occurred_at.clone(),
                        run_id: envelope.run_id.clone(),
                        role: "tool".to_owned(),
                        content: Some(
                            serde_json::json!({
                                "error": tool.message,
                                "errorCode": tool.error_code,
                            })
                            .to_string(),
                        ),
                        reasoning: None,
                        tool_calls: Vec::new(),
                        tool_call_id: Some(tool.call_id.clone()),
                        name: Some(tool.name.clone()),
                        model: None,
                        usage: None,
                        compact_up_to_cursor: None,
                    },
                });
            }
            CanonicalEvent::SessionCompacted(compacted) => {
                let Some(summary) = compacted.summary.as_ref().filter(|text| !text.is_empty())
                else {
                    continue;
                };
                reasoning.clear();
                group_start = None;
                pending_usage = None;
                entries.push(HistoryEntry {
                    start_cursor: *cursor,
                    item: HistoryItem {
                        cursor: *cursor,
                        occurred_at: envelope.occurred_at.clone(),
                        run_id: envelope.run_id.clone(),
                        role: "summary".to_owned(),
                        content: Some(summary.clone()),
                        reasoning: None,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        name: None,
                        model: None,
                        usage: None,
                        compact_up_to_cursor: compacted.compact_up_to_cursor,
                    },
                });
            }
            _ => {}
        }
    }
    entries
}

fn bounded_history(
    entries: Vec<HistoryEntry>,
    limit: usize,
    has_older_row: bool,
    max_frame_bytes: usize,
    response_id: &RpcId,
) -> Result<SessionHistoryResult, ProtocolError> {
    let original_len = entries.len();
    // `has_more` is true when either the caller's window proved an older row
    // exists or the frame-budget trim below drops leading items.
    let mut has_more = has_older_row || entries.len() > limit;
    let mut retained = entries
        .into_iter()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let encoded_len = |candidate: &[HistoryEntry], has_more: bool| {
        serde_json::to_vec(&success(
            response_id.clone(),
            ResponsePayload::SessionHistory(SessionHistoryResult {
                items: candidate.iter().map(|entry| entry.item.clone()).collect(),
                has_more,
                // Account for the largest possible cursor so a page that just
                // fits the probe cannot overflow the real frame and tear down
                // the connection.
                next_cursor: has_more.then_some(u64::MAX),
            }),
        ))
        .map(|encoded| encoded.len())
        .unwrap_or(usize::MAX)
    };
    while !retained.is_empty() && encoded_len(&retained, has_more) > max_frame_bytes {
        retained.remove(0);
        has_more = true;
    }
    // The page could not fit even one item: fail closed rather than silently
    // returning an empty page.
    if retained.is_empty() && original_len > 0 {
        return Err(error(-32008, "outbound_frame_too_large", false));
    }
    // Only expose a cursor when older history exists; a final page reports
    // `None` so clients know they have reached the oldest item. The cursor is
    // the oldest retained group's start, so paging back never re-includes a
    // reasoning delta already delivered with its assistant item.
    let next_cursor = if has_more {
        retained.first().map(|entry| entry.start_cursor)
    } else {
        None
    };
    Ok(SessionHistoryResult {
        items: retained.into_iter().map(|entry| entry.item).collect(),
        has_more,
        next_cursor,
    })
}

fn preview_event_envelope(session_id: &str, event: CanonicalEvent) -> EventEnvelope {
    EventEnvelope {
        event_id: "event-00000000-0000-0000-0000-000000000000".to_owned(),
        schema_version: V1Version::VALUE,
        session_id: session_id.to_owned(),
        run_id: "run-00000000-0000-0000-0000-000000000000".to_owned(),
        item_id: None,
        seq: 1,
        occurred_at: "2000-01-01T00:00:00.000Z".to_owned(),
        actor: runtime_actor(),
        source: "cool-app-server-m7".to_owned(),
        causation_id: None,
        correlation_id: None,
        event,
        extensions: BTreeMap::new(),
    }
}

fn preview_event_page(envelope: EventEnvelope, has_more: bool) -> ServerFrame {
    let replay = EventPage {
        events: vec![envelope.clone()],
        next_cursor: Some(EventCursor {
            run_id: envelope.run_id.clone(),
            after_seq: Some(envelope.seq),
        }),
        has_more,
    };
    success(
        RpcId::String("x".repeat(MAX_RPC_ID_BYTES)),
        ResponsePayload::EventPage(replay),
    )
}

fn success(id: RpcId, result: ResponsePayload) -> ServerFrame {
    ServerFrame::Success(RpcSuccess {
        jsonrpc: JsonRpcV2::VALUE,
        id,
        result,
    })
}

fn failure(id: RpcId, error: ProtocolError) -> ServerFrame {
    ServerFrame::Failure(RpcFailure {
        jsonrpc: JsonRpcV2::VALUE,
        id,
        error,
    })
}

fn notification(event: EventEnvelope) -> ServerFrame {
    ServerFrame::Notification(RpcNotification {
        jsonrpc: JsonRpcV2::VALUE,
        method: RunEventMethod::VALUE,
        params: StreamFrame::Event(Box::new(event)),
    })
}

const MAX_LABEL_CHARS: usize = 200;

fn validate_label(
    first_name: &'static str,
    first: Option<&str>,
    second_name: &'static str,
    second: Option<&str>,
) -> Option<ProtocolError> {
    for (name, value) in [(first_name, first), (second_name, second)] {
        if value.is_some_and(|value| value.chars().count() > MAX_LABEL_CHARS) {
            let mut error = error(-32602, "label_too_long", false);
            error.safe_details.insert(
                "field".to_owned(),
                serde_json::Value::String(name.to_owned()),
            );
            error.safe_details.insert(
                "maxChars".to_owned(),
                serde_json::Value::Number(MAX_LABEL_CHARS.into()),
            );
            return Some(error);
        }
    }
    None
}

fn error(rpc_code: i32, cool_code: &str, retryable: bool) -> ProtocolError {
    ProtocolError {
        rpc_code,
        cool_code: cool_code.to_owned(),
        message: cool_code.replace('_', " "),
        retryable,
        safe_details: BTreeMap::new(),
    }
}

/// Host-surface failures (extension admin, settings) carry their message in
/// `safe_details` after masking secret-shaped text, so the UI can show why a read
/// or mutation failed. The message may still reference a plugin/data path (not a
/// secret); the local facade is single-user and the value is only returned to the
/// same operator.
fn masked_detail_error(rpc_code: i32, cool_code: &str, message: &str) -> ProtocolError {
    let mut protocol_error = error(rpc_code, cool_code, false);
    protocol_error.safe_details.insert(
        "detail".to_owned(),
        serde_json::Value::String(mask_secrets(message)),
    );
    protocol_error
}

/// Map a structured registry error onto the family error codes (Python REST
/// parity: 404/422/409/502 → `-32004`/`-32602`/`-32006`/`-32021`).
fn mcp_store_error_frame(error: McpStoreError) -> ProtocolError {
    match error {
        McpStoreError::Unavailable(message) => {
            masked_detail_error(-32020, "mcp_admin_unavailable", &message)
        }
        McpStoreError::NotFound(message) => {
            masked_detail_error(-32004, "mcp_registry_server_not_found", &message)
        }
        McpStoreError::NoPackages(message) => {
            masked_detail_error(-32602, "invalid_params", &message)
        }
        McpStoreError::AlreadyExists(message) => masked_detail_error(-32006, "conflict", &message),
        McpStoreError::Failed(message) => masked_detail_error(-32021, "mcp_admin_failed", &message),
    }
}

/// The persisted default system prompt, applied when a normal (non-plan) turn
/// does not supply one. An unset/empty default means no system message, and a
/// settings read failure degrades to no system prompt rather than failing the
/// turn.
async fn default_system_prompt(server: &AppServer) -> Option<String> {
    if let Some(settings) = server.inner.app_settings.as_ref()
        && let Ok(record) = settings.system_prompt().await
        && !record.prompt.trim().is_empty()
    {
        return Some(record.prompt);
    }
    // Python parity: an unset settings prompt falls back to the built-in
    // default (a runtime file, `default_system_prompt.txt`).
    Some(default_agent_system_prompt().to_owned())
}

fn store_error(value: StoreError) -> ProtocolError {
    match value {
        StoreError::IdempotencyConflict => error(-32006, "idempotency_conflict", false),
        StoreError::NotFound("session") => error(-32004, "session_not_found", false),
        StoreError::NotFound("run") => error(-32005, "run_not_found", false),
        StoreError::NotFound("approval") => error(-32011, "approval_not_found", false),
        StoreError::ActorMismatch => error(-32004, "resource_not_found", false),
        StoreError::RevisionConflict => error(-32012, "approval_revision_conflict", true),
        StoreError::AlreadyResolved => error(-32013, "approval_already_resolved", false),
        StoreError::RunNotActive => error(-32005, "run_not_active", false),
        StoreError::InvalidTransition { .. } => error(-32007, "session_run_active", true),
        StoreError::BudgetExceeded(_) => error(-32014, "budget_exceeded", false),
        StoreError::NotFound(_) => error(-32004, "resource_not_found", false),
        StoreError::Sqlite(_)
        | StoreError::Json(_)
        | StoreError::Io(_)
        | StoreError::Corrupt(_) => error(-32603, "durable_state_error", true),
    }
}

fn encode_bounded_frame(frame: ServerFrame, limit: usize) -> io::Result<Vec<u8>> {
    let encoded = serde_json::to_vec(&frame).map_err(io::Error::other)?;
    if encoded.len() <= limit {
        return Ok(encoded);
    }
    let id = match frame {
        ServerFrame::Success(response) => response.id,
        ServerFrame::Failure(response) => response.id,
        ServerFrame::Notification(_) => RpcId::Null,
    };
    let fallback = serde_json::to_vec(&failure(
        id,
        error(-32008, "outbound_frame_too_large", false),
    ))
    .map_err(io::Error::other)?;
    if fallback.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configured frame limit cannot encode a structured error",
        ));
    }
    Ok(fallback)
}

fn fingerprint<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("protocol parameters serialize deterministically")
}

fn rpc_id_from_value(value: &serde_json::Value) -> RpcId {
    match value.get("id") {
        Some(serde_json::Value::String(id)) => RpcId::String(id.clone()),
        Some(serde_json::Value::Number(id)) => id.as_i64().map_or(RpcId::Null, RpcId::Integer),
        _ => RpcId::Null,
    }
}

fn rpc_id_within_limit(id: &RpcId) -> bool {
    match id {
        RpcId::String(_) => serde_json::to_vec(id)
            .is_ok_and(|encoded| encoded.len().saturating_sub(2) <= MAX_RPC_ID_BYTES),
        RpcId::Integer(_) | RpcId::Null => true,
    }
}

fn classify_invalid_request(value: &serde_json::Value) -> i32 {
    let Some(object) = value.as_object() else {
        return -32600;
    };
    let allowed = ["jsonrpc", "id", "method", "params"];
    let valid_id = matches!(
        object.get("id"),
        Some(serde_json::Value::String(_) | serde_json::Value::Null)
    ) || object
        .get("id")
        .and_then(serde_json::Value::as_i64)
        .is_some();
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || !valid_id
        || !object.contains_key("params")
    {
        -32600
    } else if object.get("method").and_then(serde_json::Value::as_str) != Some(RPC_METHOD) {
        -32601
    } else {
        -32602
    }
}

enum BoundedLine {
    Line(Vec<u8>),
    TooLarge,
    Eof,
}

async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> io::Result<BoundedLine>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    let mut overflow = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() && !overflow {
                Ok(BoundedLine::Eof)
            } else if overflow {
                Ok(BoundedLine::TooLarge)
            } else {
                Ok(BoundedLine::Line(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if !overflow {
            let payload_len = if newline.is_some() {
                consumed.saturating_sub(1)
            } else {
                consumed
            };
            if line.len() + payload_len > limit {
                overflow = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..payload_len]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            return if overflow {
                Ok(BoundedLine::TooLarge)
            } else {
                Ok(BoundedLine::Line(line))
            };
        }
    }
}

struct StdioIo<R, W> {
    reader: R,
    writer: W,
}

impl<R: AsyncRead + Unpin, W: Unpin> AsyncRead for StdioIo<R, W> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl<R: Unpin, W: AsyncWrite + Unpin> AsyncWrite for StdioIo<R, W> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, io::Error>> {
        std::pin::Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), io::Error>> {
        std::pin::Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

/// Built-in recurring-workflow templates (the Rust mirror of the Python
/// `app.tasks.templates.TASK_TEMPLATES` catalog). The catalog is static data
/// owned by the runtime; it carries no secrets and does not touch the store.
pub fn task_templates() -> Vec<TaskTemplateRecord> {
    fn template(
        slug: &str,
        name: &str,
        description: &str,
        prompt: &str,
        cron_expression: &str,
        tools_whitelist: &[&str],
        max_iterations: i64,
    ) -> TaskTemplateRecord {
        TaskTemplateRecord {
            slug: slug.to_owned(),
            name: name.to_owned(),
            description: description.to_owned(),
            prompt: prompt.to_owned(),
            cron_expression: cron_expression.to_owned(),
            tools_whitelist: Some(
                tools_whitelist
                    .iter()
                    .map(|tool| (*tool).to_owned())
                    .collect(),
            ),
            max_iterations,
            delivery_channels: vec!["ui".to_owned()],
        }
    }
    vec![
        template(
            "news-digest",
            "Daily news / research digest",
            "Search the web for updates on your topics and produce a short digest.",
            "Prepare a concise daily digest of notable news and research on my \
             topics of interest. Search the web, group findings by theme, keep \
             each item to one or two sentences, and include source links.",
            "0 8 * * *",
            &["web_search", "web_fetch", "memory_recall"],
            12,
        ),
        template(
            "code-review",
            "Code review / cleanup",
            "Review recent changes in the working directory and report issues.",
            "Review the code in my working directory. Look for bugs, dead code, \
             missing error handling and style violations. Report the findings \
             grouped by file, most important first, with concrete suggestions.",
            "0 18 * * 1-5",
            &["read_file", "list_files"],
            15,
        ),
        template(
            "memory-review",
            "Memory review",
            "Periodically revisit long-term memory: stale, duplicate or unconfirmed items.",
            "Review my long-term memory. Recall the most important stored items, \
             point out anything stale, duplicated or contradictory, and suggest \
             what should be updated or forgotten. Do not delete anything yourself.",
            "0 9 * * 1",
            &["memory_recall", "memory_list"],
            8,
        ),
        template(
            "health-check",
            "Health check / monitoring",
            "Probe the configured endpoints and report anything unhealthy.",
            "Check that my monitored endpoints respond correctly. Report status, \
             latency and any failures. Keep the report to a few lines when \
             everything is healthy.",
            "0 */6 * * *",
            &["web_fetch"],
            6,
        ),
    ]
}

pub fn capabilities() -> BTreeSet<String> {
    [
        "approval_resolution",
        "agent_loop",
        "conversation_sessions",
        "durable_sessions",
        "event_catch_up",
        "extension_admin",
        "extension_admin_write",
        "local_socket",
        "mcp_admin",
        "mcp_store",
        "memory_extract",
        "plan_execution",
        "provider_probe",
        "rss_fetch",
        "recovery",
        "run_cancellation",
        "run_subscribe",
        "session_fork",
        "settings",
        "session_history",
        "session_history_cursor",
        "session_list",
        "session_runs",
        "session_steer",
        "skills_admin",
        "streaming_models",
        "stdio",
        "task_templates",
        "tool_catalog",
        "trusted_tools",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn rfc3339_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let seconds = elapsed.as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    )
}

// Gregorian civil date from Unix epoch days, following Howard Hinnant's public-domain algorithm.
fn civil_date(days_since_epoch: i64) -> (i64, u32, u32) {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cool_protocol::{
        CanonicalEvent, ItemEvent, RpcId, RunTerminal, ServerFrame, StreamFrame, ToolRequested,
    };

    use super::{
        AppServer, Outbound, ServerConfig, civil_date, error, failure, notification,
        preview_event_envelope, preview_event_page,
    };

    #[test]
    fn unix_day_conversion_covers_epoch_and_leap_day() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(19_782), (2024, 2, 29));
    }

    #[test]
    fn replay_preflight_uses_the_exact_boundary_for_a_terminal_page() {
        let session_id = "session-00000000-0000-0000-0000-000000000000";
        let event = CanonicalEvent::RunCompleted(RunTerminal {
            reason: "stop".to_owned(),
            error_code: None,
        });
        let envelope = preview_event_envelope(session_id, event.clone());
        let live_len = serde_json::to_vec(&notification(envelope.clone()))
            .expect("notification serializes")
            .len();
        let non_terminal_len = serde_json::to_vec(&preview_event_page(envelope.clone(), true))
            .expect("non-terminal page serializes")
            .len();
        let terminal_len = serde_json::to_vec(&preview_event_page(envelope, false))
            .expect("terminal page serializes")
            .len();
        assert_eq!(terminal_len, non_terminal_len + 1);
        assert!(live_len <= non_terminal_len);

        let server = AppServer::new(ServerConfig {
            max_frame_bytes: non_terminal_len,
            ..ServerConfig::default()
        });
        assert!(!server.preview_event_frame_fits(session_id, event));
    }

    #[tokio::test]
    async fn queue_enqueue_timeout_marks_the_connection_failed() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(failure(RpcId::Integer(1), error(-32600, "occupied", false)))
            .unwrap();
        let (failed, mut failed_rx) = tokio::sync::watch::channel(false);
        let outbound = Outbound {
            sender,
            failed,
            deadline: Duration::from_millis(10),
        };
        assert!(
            !outbound
                .send(failure(
                    RpcId::Integer(2),
                    error(-32600, "must_timeout", false),
                ))
                .await
        );
        failed_rx.changed().await.unwrap();
        assert!(*failed_rx.borrow());
    }

    #[tokio::test]
    async fn internal_disconnect_closes_pending_tools_before_cancellation() {
        let server = AppServer::new(ServerConfig::default());
        let session = server
            .inner
            .store
            .create_session(
                "local-user",
                "internal-session",
                "internal-session",
                None,
                None,
            )
            .unwrap()
            .value;
        let run = server
            .inner
            .store
            .start_run("local-user", "internal-run", "internal-run", &session)
            .unwrap()
            .value;
        server
            .append_event(
                &run,
                CanonicalEvent::ItemCompleted(ItemEvent {
                    role: Some("assistant".to_owned()),
                    content: None,
                    tool_calls: vec![ToolRequested {
                        call_id: "pending-on-disconnect".to_owned(),
                        name: "write_file".to_owned(),
                        arguments: Default::default(),
                    }],
                }),
                false,
            )
            .await
            .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let (failed, _) = tokio::sync::watch::channel(false);
        let outbound = Outbound {
            sender,
            failed,
            deadline: Duration::from_secs(1),
        };
        assert!(
            server
                .finish_cancelled(&run, "disconnect", Some(&outbound))
                .await
        );
        let delivered = [
            receiver.recv().await.unwrap(),
            receiver.recv().await.unwrap(),
        ]
        .into_iter()
        .map(|frame| match frame {
            ServerFrame::Notification(notification) => match notification.params {
                StreamFrame::Event(event) => event.event,
                other => panic!("expected event notification, got {other:?}"),
            },
            other => panic!("expected notification, got {other:?}"),
        })
        .collect::<Vec<_>>();
        assert!(matches!(delivered[0], CanonicalEvent::ToolFailed(_)));
        assert!(matches!(delivered[1], CanonicalEvent::RunCancelled(_)));
        let events = server.inner.store.all_events(&run, "local-user").unwrap();
        assert!(matches!(events[1].event, CanonicalEvent::ToolFailed(_)));
        assert!(matches!(events[2].event, CanonicalEvent::RunCancelled(_)));
    }
}
