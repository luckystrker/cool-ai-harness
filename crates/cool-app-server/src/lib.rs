//! M7 App Protocol server backed by the durable Rust agent runtime.
//!
//! This crate owns transport/session plumbing only. Provider, tool and policy
//! decisions remain delegated to `cool-agent`, `cool-security` and `cool-state`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, ApprovalGate, ApprovalRequest, CancelSignal,
    EventSink, Message, RunOutcome, RuntimeError, ScriptedDriver, ToolContext, Usage,
    builtin_registry, history_from_events, mask_canonical_event,
};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalResolvedResult, CanonicalEvent, Command, ContentPart, EventCursor,
    EventEnvelope, EventPage, InitializeResult, JsonRpcV2, PromptAcceptedResult, ProtocolError,
    ResponsePayload, RpcFailure, RpcId, RpcNotification, RpcRequest, RpcSuccess,
    RunCancelledResult, RunEventMethod, RunStarted, RunTerminal, ServerFrame, SessionCreatedResult,
    SessionLoadedResult, StreamFrame, TextDelta, TransportLimits, V1Version,
};
use cool_security::{CapabilityPolicy, Decision, Workspace, mask_secrets};
use cool_state::{BudgetDelta, CancelAcceptance, DurableStore, EventProvenance, StoreError};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_RPC_ID_BYTES: usize = 128;
pub const RPC_METHOD: &str = "cool.command";
pub const EVENT_METHOD: &str = "run.event";

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub max_frame_bytes: usize,
    pub max_in_flight: usize,
    pub outbound_queue: usize,
    pub event_page_limit: u16,
    pub delivery_timeout: Duration,
    pub write_timeout: Duration,
    pub event_delay: Duration,
    pub request_delay: Duration,
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
        }
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
    lifecycle: Option<Arc<dyn RunLifecycle>>,
}

#[async_trait]
pub trait RunLifecycle: Send + Sync {
    async fn on_event(
        &self,
        event: &str,
        payload: serde_json::Value,
        policy: &CapabilityPolicy,
    ) -> Vec<CanonicalEvent>;
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

#[derive(Default)]
struct ConnectionState {
    initialized: bool,
    owned_runs: HashSet<String>,
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
                lifecycle: None,
            }),
        }
    }

    pub fn with_run_lifecycle(mut self, lifecycle: Arc<dyn RunLifecycle>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("run lifecycle must be configured before the server is cloned")
            .lifecycle = Some(lifecycle);
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

        let connection = Arc::new(Mutex::new(ConnectionState::default()));
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
                            self.dispatch(request, outbound.clone(), connection.clone())
                                .await;
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
                        server.dispatch(request, outbound, connection).await;
                    });
                }
            }
        }

        while handlers.join_next().await.is_some() {}
        let owned_runs = connection.lock().await.owned_runs.clone();
        for run_id in owned_runs {
            self.signal_cancel(&run_id, "disconnect").await;
        }
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
                            connection.lock().await.owned_runs.insert(run_id.clone());
                            self.spawn_agent_run(run_id, content, params.model, cancel, outbound);
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
                            let _ = self.send(&outbound, notification(resolution.event)).await;
                        }
                    }
                    Err(store) => {
                        let _ = self.send(&outbound, failure(id, store_error(store))).await;
                    }
                }
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

    async fn send(&self, outbound: &Outbound, frame: ServerFrame) -> bool {
        outbound.send(frame).await
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
        content: String,
        model: Option<String>,
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
            };
            let approvals = AppServerApprovalGate {
                server: server.clone(),
                run_id: run_id.clone(),
                session_id: run.session_id,
                outbound: outbound.clone(),
            };
            let masked_prompt = mask_secrets(&content);
            let request = AgentRequest {
                model: model.unwrap_or_else(|| server.inner.default_model.clone()),
                history: Vec::<Message>::new(),
                user_input: content,
                system_prompt: None,
                temperature: 0.0,
                max_tokens: None,
                limits: AgentLimits::default(),
                tool_names: None,
                tool_context: ToolContext::new(
                    server.inner.workspace.clone(),
                    server.inner.policy.clone(),
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
                if let Some(outbound) = outbound {
                    for event in acceptance.events {
                        if !self.send(outbound, notification(event)).await {
                            break;
                        }
                    }
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
        let event = mask_canonical_event(event)?;
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

    async fn load_history(&self) -> Result<Vec<Message>, RuntimeError> {
        let run = self
            .server
            .inner
            .store
            .run(&self.run_id, &local_actor().id)?;
        history_from_events(
            &self
                .server
                .inner
                .store
                .session_events(&run.session_id, &local_actor().id)?,
        )
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

fn local_actor() -> ActorRef {
    ActorRef {
        id: "local-user".to_owned(),
        kind: ActorKind::LocalUser,
    }
}

fn runtime_actor() -> ActorRef {
    ActorRef {
        id: "cool-app-server".to_owned(),
        kind: ActorKind::System,
    }
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

fn error(rpc_code: i32, cool_code: &str, retryable: bool) -> ProtocolError {
    ProtocolError {
        rpc_code,
        cool_code: cool_code.to_owned(),
        message: cool_code.replace('_', " "),
        retryable,
        safe_details: BTreeMap::new(),
    }
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

pub fn capabilities() -> BTreeSet<String> {
    [
        "approval_resolution",
        "agent_loop",
        "durable_sessions",
        "event_catch_up",
        "local_socket",
        "recovery",
        "run_cancellation",
        "streaming_models",
        "stdio",
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
