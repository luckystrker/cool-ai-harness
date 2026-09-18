//! App Protocol client for Rust clients (TUI, ACP adapter, tests).
//!
//! The client speaks the versioned JSON-RPC transport only: it never touches
//! `cool-state` or the agent runtime. Durable catch-up uses `run.events`
//! pagination so a reconnect cannot lose or duplicate canonical events.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use cool_protocol::{
    ApprovalDecision, ApprovalResolvedResult, Command, CommandEnvelope, ContentPart, EventEnvelope,
    EventPage, InitializeResult, JsonRpcV2, PromptAcceptedResult, ProtocolError, ResponsePayload,
    RpcId, RpcRequest, RunCancelledResult, ServerFrame, SessionForkedResult, SessionHistoryResult,
    SessionListResult, SessionLoadedResult, StatusGetResult, SteerAcceptedResult, StreamFrame,
    V1Version,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast, oneshot, watch};
use tokio::time::timeout;
use uuid::Uuid;

const CLIENT_NAME: &str = "cool-client";
const EVENT_BUFFER: usize = 512;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_PAGE_LIMIT: u16 = 128;

#[derive(Debug)]
pub enum ClientError {
    Protocol(ProtocolError),
    Transport(String),
    Timeout,
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(formatter, "{}: {}", error.cool_code, error.message),
            Self::Transport(message) => write!(formatter, "transport error: {message}"),
            Self::Timeout => formatter.write_str("protocol request timed out"),
            Self::UnexpectedResponse { expected, actual } => {
                write!(formatter, "expected {expected} response, got {actual}")
            }
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ProtocolError> for ClientError {
    fn from(value: ProtocolError) -> Self {
        Self::Protocol(value)
    }
}

type Pending = oneshot::Sender<Result<ResponsePayload, ProtocolError>>;

struct Inner {
    writer: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
    events: broadcast::Sender<EventEnvelope>,
    next_id: AtomicI64,
    request_timeout: Duration,
    closed: watch::Sender<bool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Releases the reader task, which in turn drops the read half so a
        // split transport observes EOF instead of staying half-open.
        let _ = self.closed.send(true);
    }
}

/// One live App Protocol session. Dropping the client closes the transport.
#[derive(Clone)]
pub struct AppClient {
    inner: Arc<Inner>,
    command_prefix: String,
}

impl AppClient {
    pub fn connect<R, W>(reader: R, writer: W) -> io::Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_with_buffer(reader, writer, EVENT_BUFFER)
    }

    /// Connects with an explicit live-event buffer. Small buffers are used by
    /// tests to exercise durable catch-up after `Lagged`.
    pub fn connect_with_buffer<R, W>(reader: R, writer: W, capacity: usize) -> io::Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        assert!(capacity > 0, "event buffer must be positive");
        let (events, _) = broadcast::channel(capacity);
        let (closed, mut closed_rx) = watch::channel(false);
        // The reader task deliberately holds no reference to the writer, so
        // dropping the last `AppClient` closes the transport and lets the peer
        // observe EOF even while this task is still draining the read half.
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let reader_events = events.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = Vec::new();
            let failure = loop {
                line.clear();
                let read = tokio::select! {
                    read = reader.read_until(b'\n', &mut line) => read,
                    _ = closed_rx.changed() => break "client closed".to_owned(),
                };
                match read {
                    Ok(0) => break "transport closed before a response".to_owned(),
                    Ok(_) => {}
                    Err(error) => break format!("transport read failed: {error}"),
                }
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                match serde_json::from_slice::<ServerFrame>(&line) {
                    Ok(ServerFrame::Success(success)) => {
                        if let Some(sender) = take_pending(&reader_pending, &success.id).await {
                            let _ = sender.send(Ok(success.result));
                        }
                    }
                    Ok(ServerFrame::Failure(failure)) => {
                        if let Some(sender) = take_pending(&reader_pending, &failure.id).await {
                            let _ = sender.send(Err(failure.error));
                        }
                    }
                    Ok(ServerFrame::Notification(notification)) => {
                        if let StreamFrame::Event(envelope) = notification.params {
                            let _ = reader_events.send(*envelope);
                        }
                    }
                    Err(error) => break format!("invalid server frame: {error}"),
                }
            };
            fail_pending(&reader_pending, &failure).await;
        });
        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(Box::new(writer)),
                pending,
                events,
                next_id: AtomicI64::new(1),
                request_timeout: DEFAULT_REQUEST_TIMEOUT,
                closed,
            }),
            command_prefix: format!("{CLIENT_NAME}-{}", std::process::id()),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.inner.events.subscribe()
    }

    pub async fn request(&self, command: Command) -> Result<ResponsePayload, ClientError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let envelope = CommandEnvelope {
            protocol_version: V1Version::VALUE,
            command_id: format!("{}-{id}", self.command_prefix),
            command,
        };
        let request = RpcRequest {
            jsonrpc: JsonRpcV2::VALUE,
            id: RpcId::Integer(id),
            method: cool_protocol::CoolCommandMethod::VALUE,
            params: envelope,
        };
        let mut encoded = serde_json::to_vec(&request)
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        encoded.push(b'\n');
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, sender);
        {
            let mut writer = self.inner.writer.lock().await;
            if let Err(error) = writer.write_all(&encoded).await {
                self.inner.pending.lock().await.remove(&id);
                return Err(ClientError::Transport(error.to_string()));
            }
            if let Err(error) = writer.flush().await {
                self.inner.pending.lock().await.remove(&id);
                return Err(ClientError::Transport(error.to_string()));
            }
        }
        match timeout(self.inner.request_timeout, receiver).await {
            Ok(Ok(result)) => result.map_err(ClientError::Protocol),
            Ok(Err(_)) => Err(ClientError::Transport(
                "response channel closed before delivery".to_owned(),
            )),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(ClientError::Timeout)
            }
        }
    }

    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<InitializeResult, ClientError> {
        let response = self
            .request(Command::Initialize(cool_protocol::InitializeParams {
                client_name: client_name.to_owned(),
                client_version: client_version.to_owned(),
                supported_protocol_versions: vec![1],
                capabilities: Default::default(),
            }))
            .await?;
        match response {
            ResponsePayload::Initialized(result) => Ok(result),
            other => Err(unexpected("initialized", &other)),
        }
    }

    pub async fn create_session(
        &self,
        key: &str,
        title: Option<&str>,
        project_key: Option<&str>,
    ) -> Result<String, ClientError> {
        let response = self
            .request(Command::SessionCreate(cool_protocol::SessionCreateParams {
                idempotency_key: idempotency(key)?,
                title: title.map(str::to_owned),
                project_key: project_key.map(str::to_owned),
            }))
            .await?;
        match response {
            ResponsePayload::SessionCreated(result) => Ok(result.session_id),
            other => Err(unexpected("session_created", &other)),
        }
    }

    pub async fn load_session(&self, session_id: &str) -> Result<SessionLoadedResult, ClientError> {
        let response = self
            .request(Command::SessionLoad(cool_protocol::SessionLoadParams {
                session_id: session_id.to_owned(),
            }))
            .await?;
        match response {
            ResponsePayload::SessionLoaded(result) => Ok(result),
            other => Err(unexpected("session_loaded", &other)),
        }
    }

    pub async fn list_sessions(
        &self,
        project_key: Option<&str>,
        limit: u16,
    ) -> Result<SessionListResult, ClientError> {
        let response = self
            .request(Command::SessionList(cool_protocol::SessionListParams {
                project_key: project_key.map(str::to_owned),
                limit,
            }))
            .await?;
        match response {
            ResponsePayload::SessionListed(result) => Ok(result),
            other => Err(unexpected("session_listed", &other)),
        }
    }

    pub async fn session_history(
        &self,
        session_id: &str,
        limit: u16,
    ) -> Result<SessionHistoryResult, ClientError> {
        let response = self
            .request(Command::SessionHistory(
                cool_protocol::SessionHistoryParams {
                    session_id: session_id.to_owned(),
                    limit,
                },
            ))
            .await?;
        match response {
            ResponsePayload::SessionHistory(result) => Ok(result),
            other => Err(unexpected("session_history", &other)),
        }
    }

    pub async fn fork_session(
        &self,
        key: &str,
        session_id: &str,
        title: Option<&str>,
    ) -> Result<SessionForkedResult, ClientError> {
        let response = self
            .request(Command::SessionFork(cool_protocol::SessionForkParams {
                idempotency_key: idempotency(key)?,
                session_id: session_id.to_owned(),
                title: title.map(str::to_owned),
            }))
            .await?;
        match response {
            ResponsePayload::SessionForked(result) => Ok(result),
            other => Err(unexpected("session_forked", &other)),
        }
    }

    pub async fn prompt(
        &self,
        key: &str,
        session_id: &str,
        text: &str,
        model: Option<&str>,
    ) -> Result<PromptAcceptedResult, ClientError> {
        let response = self
            .request(Command::SessionPrompt(cool_protocol::SessionPromptParams {
                idempotency_key: idempotency(key)?,
                session_id: session_id.to_owned(),
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
                model: model.map(str::to_owned),
            }))
            .await?;
        match response {
            ResponsePayload::PromptAccepted(result) => Ok(result),
            other => Err(unexpected("prompt_accepted", &other)),
        }
    }

    pub async fn steer(
        &self,
        key: &str,
        run_id: &str,
        text: &str,
    ) -> Result<SteerAcceptedResult, ClientError> {
        let response = self
            .request(Command::SessionSteer(cool_protocol::SessionSteerParams {
                idempotency_key: idempotency(key)?,
                run_id: run_id.to_owned(),
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
            }))
            .await?;
        match response {
            ResponsePayload::SteerAccepted(result) => Ok(result),
            other => Err(unexpected("steer_accepted", &other)),
        }
    }

    pub async fn cancel_run(
        &self,
        key: &str,
        run_id: &str,
        reason: Option<&str>,
    ) -> Result<RunCancelledResult, ClientError> {
        let response = self
            .request(Command::RunCancel(cool_protocol::RunCancelParams {
                idempotency_key: idempotency(key)?,
                run_id: run_id.to_owned(),
                reason: reason.map(str::to_owned),
            }))
            .await?;
        match response {
            ResponsePayload::RunCancelled(result) => Ok(result),
            other => Err(unexpected("run_cancelled", &other)),
        }
    }

    pub async fn run_events(
        &self,
        run_id: &str,
        after_seq: Option<u64>,
        limit: u16,
    ) -> Result<EventPage, ClientError> {
        let response = self
            .request(Command::RunEvents(cool_protocol::RunEventsParams {
                run_id: run_id.to_owned(),
                after_seq,
                limit,
            }))
            .await?;
        match response {
            ResponsePayload::EventPage(page) => Ok(page),
            other => Err(unexpected("event_page", &other)),
        }
    }

    pub async fn resolve_approval(
        &self,
        key: &str,
        approval_id: &str,
        revision: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalResolvedResult, ClientError> {
        let response = self
            .request(Command::ApprovalResolve(
                cool_protocol::ApprovalResolveParams {
                    idempotency_key: idempotency(key)?,
                    approval_id: approval_id.to_owned(),
                    expected_revision: revision,
                    decision,
                },
            ))
            .await?;
        match response {
            ResponsePayload::ApprovalResolved(result) => Ok(result),
            other => Err(unexpected("approval_resolved", &other)),
        }
    }

    pub async fn status(&self) -> Result<StatusGetResult, ClientError> {
        let response = self
            .request(Command::StatusGet(cool_protocol::StatusGetParams {}))
            .await?;
        match response {
            ResponsePayload::Status(result) => Ok(result),
            other => Err(unexpected("status", &other)),
        }
    }
}

pub fn new_idempotency_key(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

fn idempotency(value: &str) -> Result<cool_protocol::IdempotencyKey, ClientError> {
    cool_protocol::IdempotencyKey::new(value)
        .map_err(|error| ClientError::Transport(format!("invalid idempotency key: {error}")))
}

fn unexpected(expected: &'static str, actual: &ResponsePayload) -> ClientError {
    ClientError::UnexpectedResponse {
        expected,
        actual: format!("{actual:?}"),
    }
}

async fn take_pending(pending: &Mutex<HashMap<i64, Pending>>, id: &RpcId) -> Option<Pending> {
    let RpcId::Integer(id) = id else {
        return None;
    };
    pending.lock().await.remove(id)
}

async fn fail_pending(pending: &Mutex<HashMap<i64, Pending>>, message: &str) {
    let pending = std::mem::take(&mut *pending.lock().await);
    for (_, sender) in pending {
        let _ = sender.send(Err(ProtocolError {
            rpc_code: -32000,
            cool_code: "transport_closed".to_owned(),
            message: message.to_owned(),
            retryable: true,
            safe_details: Default::default(),
        }));
    }
}
