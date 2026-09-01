use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cool_security::sanitize_environment;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, watch};
use tokio::time::timeout;

use cool_protocol::{CanonicalEvent, WorkerEvent};

const MAX_WORKER_MESSAGE: usize = 1_048_576;
const CORE_WORKER_CAPABILITIES: &[&str] = &[
    "request",
    "cancel",
    "heartbeat",
    "shutdown",
    "deadlines",
    "structured_errors",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityAdapter {
    Codex,
    Claude,
}

#[derive(Clone, Debug)]
pub struct WorkerLaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub allowed_secret_environment: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerOperationClass {
    ReadOnly,
    SideEffect,
}

#[derive(Debug)]
pub enum WorkerRequestOutcome {
    Completed {
        value: Value,
        events: Vec<CanonicalEvent>,
    },
    /// The request may have reached the worker. The supervisor restarted the worker but did not
    /// replay the request, even when the caller supplied an idempotency key.
    UnknownOutcome {
        error: String,
        events: Vec<CanonicalEvent>,
    },
}

#[derive(Clone, Default)]
pub struct CompatibilityWorkerSupervisor {
    workers: Arc<Mutex<BTreeMap<CompatibilityAdapter, ManagedWorker>>>,
}

struct ManagedWorker {
    spec: WorkerLaunchSpec,
    protocol: WorkerProtocol,
    attempt: u32,
}

impl CompatibilityWorkerSupervisor {
    pub async fn start(
        &self,
        adapter: CompatibilityAdapter,
        spec: WorkerLaunchSpec,
    ) -> Result<CanonicalEvent, WorkerError> {
        let protocol = spawn_from_spec(adapter, &spec).await?;
        self.workers.lock().await.insert(
            adapter,
            ManagedWorker {
                spec,
                protocol,
                attempt: 1,
            },
        );
        Ok(worker_event(adapter, 1, "started", None))
    }

    pub async fn request(
        &self,
        adapter: CompatibilityAdapter,
        method: &str,
        params: Value,
        class: WorkerOperationClass,
        idempotency_key: Option<&str>,
    ) -> Result<WorkerRequestOutcome, WorkerError> {
        if class == WorkerOperationClass::SideEffect
            && idempotency_key.is_none_or(|key| key.trim().is_empty())
        {
            return Err(WorkerError::Protocol(
                "side-effecting worker request requires an idempotency key".to_owned(),
            ));
        }
        let mut workers = self.workers.lock().await;
        let worker = workers.get_mut(&adapter).ok_or(WorkerError::Exited)?;
        let (method, payload) = adapter.translate_request(
            method,
            serde_json::json!({"input": params, "idempotencyKey": idempotency_key}),
        );
        match worker.protocol.request(&method, payload).await {
            Ok(value) => Ok(WorkerRequestOutcome::Completed {
                value: adapter.translate_response(value)?,
                events: Vec::new(),
            }),
            Err(error) => {
                let failed =
                    worker_event(adapter, worker.attempt, "failed", Some(error.to_string()));
                worker.protocol.terminate().await;
                worker.attempt = worker.attempt.saturating_add(1);
                worker.protocol = spawn_from_spec(adapter, &worker.spec).await?;
                let restarted = worker_event(adapter, worker.attempt, "restarted", None);
                Ok(WorkerRequestOutcome::UnknownOutcome {
                    error: error.to_string(),
                    events: vec![failed, restarted],
                })
            }
        }
    }

    pub async fn heartbeat(&self) -> Vec<CanonicalEvent> {
        let mut workers = self.workers.lock().await;
        let mut events = Vec::new();
        for (adapter, worker) in workers.iter_mut() {
            if worker.protocol.request("ping", Value::Null).await.is_err() {
                events.push(worker_event(
                    *adapter,
                    worker.attempt,
                    "failed",
                    Some("heartbeat_failed".to_owned()),
                ));
                worker.protocol.terminate().await;
                worker.attempt = worker.attempt.saturating_add(1);
                match spawn_from_spec(*adapter, &worker.spec).await {
                    Ok(protocol) => {
                        worker.protocol = protocol;
                        events.push(worker_event(*adapter, worker.attempt, "restarted", None));
                    }
                    Err(error) => events.push(worker_event(
                        *adapter,
                        worker.attempt,
                        "failed",
                        Some(error.to_string()),
                    )),
                }
            }
        }
        events
    }
}

async fn spawn_from_spec(
    adapter: CompatibilityAdapter,
    spec: &WorkerLaunchSpec,
) -> Result<WorkerProtocol, WorkerError> {
    WorkerProtocol::spawn(
        adapter,
        spec.program.clone(),
        spec.args.clone(),
        spec.cwd.clone(),
        spec.environment.clone(),
        spec.allowed_secret_environment.clone(),
    )
    .await
}

fn worker_event(
    adapter: CompatibilityAdapter,
    attempt: u32,
    state: &str,
    code: Option<String>,
) -> CanonicalEvent {
    let payload = WorkerEvent {
        worker_id: format!("{}-compatibility", adapter.protocol_name()),
        attempt,
        code,
    };
    match state {
        "started" => CanonicalEvent::WorkerStarted(payload),
        "restarted" => CanonicalEvent::WorkerRestarted(payload),
        _ => CanonicalEvent::WorkerFailed(payload),
    }
}

impl CompatibilityAdapter {
    pub fn protocol_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn capability(self) -> &'static str {
        match self {
            Self::Codex => "codex_protocol",
            Self::Claude => "claude_protocol",
        }
    }

    fn translate_request(self, operation: &str, input: Value) -> (String, Value) {
        (
            format!("{}.request", self.protocol_name()),
            serde_json::json!({"operation": operation, "input": input}),
        )
    }

    fn translate_response(self, response: Value) -> Result<Value, WorkerError> {
        let field = match self {
            Self::Codex => "output",
            Self::Claude => "content",
        };
        response.get(field).cloned().ok_or_else(|| {
            WorkerError::Protocol(format!(
                "{} adapter response is missing {field}",
                self.protocol_name()
            ))
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerRpcError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerResponse {
    pub id: u64,
    pub ok: bool,
    pub result: Option<Value>,
    pub error: Option<WorkerRpcError>,
}

#[derive(Debug)]
pub enum WorkerError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Protocol(String),
    Remote(WorkerRpcError),
    Timeout,
    Cancelled,
    Exited,
}

impl fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "worker I/O error: {error}"),
            Self::Json(error) => write!(formatter, "worker JSON error: {error}"),
            Self::Protocol(message) => write!(formatter, "worker protocol error: {message}"),
            Self::Remote(error) => write!(
                formatter,
                "worker remote error {}: {} (retryable={})",
                error.code, error.message, error.retryable
            ),
            Self::Timeout => formatter.write_str("worker request timed out"),
            Self::Cancelled => formatter.write_str("worker request was cancelled"),
            Self::Exited => formatter.write_str("worker exited"),
        }
    }
}

impl std::error::Error for WorkerError {}
impl From<std::io::Error> for WorkerError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for WorkerError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub struct WorkerProtocol {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    timeout: Duration,
    adapter: CompatibilityAdapter,
}

impl WorkerProtocol {
    pub async fn spawn(
        adapter: CompatibilityAdapter,
        program: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
        environment: BTreeMap<String, String>,
        allowed_secret_environment: BTreeSet<String>,
    ) -> Result<Self, WorkerError> {
        let mut safe_environment = crate::execution_environment_baseline();
        safe_environment.extend(sanitize_environment(
            environment
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            &allowed_secret_environment,
        ));
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(safe_environment)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker stdin missing".to_owned()))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| WorkerError::Protocol("worker stdout missing".to_owned()))?,
        );
        let mut worker = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            timeout: Duration::from_secs(30),
            adapter,
        };
        let response = match worker
            .request(
                "handshake",
                serde_json::json!({
                    "protocolVersion":1,
                    "adapter":adapter.protocol_name(),
                    "capabilities": CORE_WORKER_CAPABILITIES
                        .iter()
                        .copied()
                        .chain(std::iter::once(adapter.capability()))
                        .collect::<Vec<_>>()
                }),
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                worker.terminate().await;
                return Err(error);
            }
        };
        let capabilities = response
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>()
            });
        if response.get("protocolVersion").and_then(Value::as_u64) != Some(1)
            || capabilities.as_ref().is_none_or(|capabilities| {
                CORE_WORKER_CAPABILITIES
                    .iter()
                    .chain(std::iter::once(&adapter.capability()))
                    .any(|capability| !capabilities.contains(capability))
            })
        {
            worker.terminate().await;
            return Err(WorkerError::Protocol(
                "worker protocol version mismatch".to_owned(),
            ));
        }
        Ok(worker)
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, WorkerError> {
        let (_cancel, receiver) = watch::channel(false);
        self.request_cancellable(method, params, receiver).await
    }

    pub async fn request_cancellable(
        &mut self,
        method: &str,
        params: Value,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value, WorkerError> {
        if method.is_empty() {
            return Err(WorkerError::Protocol("worker method is empty".to_owned()));
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| WorkerError::Protocol("worker request id overflow".to_owned()))?;
        let deadline_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .saturating_add(self.timeout.as_millis());
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "id":id,
            "method":method,
            "params":params,
            "adapter":self.adapter.protocol_name(),
            "deadlineUnixMs":deadline_unix_ms,
        }))?;
        if bytes.len() > MAX_WORKER_MESSAGE {
            return Err(WorkerError::Protocol(
                "worker request is too large".to_owned(),
            ));
        }
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        timeout(self.timeout, async {
            loop {
                let line = tokio::select! {
                    changed = cancel.changed() => {
                        if changed.is_ok() && *cancel.borrow() {
                            self.send_cancel(id).await?;
                            return Err(WorkerError::Cancelled);
                        }
                        continue;
                    }
                    line = read_bounded_line(&mut self.stdout) => line?,
                };
                if line.is_empty() {
                    return Err(WorkerError::Exited);
                }
                let response: WorkerResponse = serde_json::from_slice(&line)?;
                if response.id != id {
                    continue;
                }
                if response.ok {
                    return Ok(response.result.unwrap_or(Value::Null));
                }
                return Err(response.error.map_or_else(
                    || WorkerError::Protocol("worker failed without structured error".to_owned()),
                    WorkerError::Remote,
                ));
            }
        })
        .await
        .map_err(|_| WorkerError::Timeout)?
    }

    async fn send_cancel(&mut self, request_id: u64) -> Result<(), WorkerError> {
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "method":"cancel",
            "params":{"requestId":request_id},
            "adapter":self.adapter.protocol_name(),
        }))?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub async fn stop(mut self) -> Result<(), WorkerError> {
        let _ = self.request("shutdown", Value::Null).await;
        if timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            self.child.kill().await?;
            self.child.wait().await?;
        }
        Ok(())
    }

    async fn terminate(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, WorkerError> {
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(output);
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len() + take > MAX_WORKER_MESSAGE {
            return Err(WorkerError::Protocol(
                "worker response is too large".to_owned(),
            ));
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if output.last() == Some(&b'\n') {
            return Ok(output);
        }
    }
}
