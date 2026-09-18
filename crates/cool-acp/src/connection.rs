//! ACP v1 JSON-RPC connection over the Rust App Protocol core.
//!
//! The adapter is a transport projection: session state, approvals and tool
//! execution stay in the durable Rust runtime behind `AppClient`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cool_app_server::AppClient;
use cool_app_server::client::new_idempotency_key;
use cool_protocol::ApprovalDecision;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

use crate::projection::AcpProjection;

pub const ACP_PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1_048_576;
pub const MAX_JSON_DEPTH: usize = 128;
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

pub struct AcpError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl AcpError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }

    pub fn session_not_found() -> Self {
        Self::new(-32002, "session not found")
    }

    fn to_json(&self, id: Value) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(data) = &self.data
            && let Some(object) = error.as_object_mut()
        {
            object.insert("data".to_owned(), data.clone());
        }
        json!({"jsonrpc": "2.0", "id": id, "error": error})
    }
}

#[derive(Clone)]
struct ActivePrompt {
    run_id: String,
}

pub struct AcpServer {
    app: AppClient,
    workspace: PathBuf,
    writer: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    initialized: AtomicBool,
    active: Mutex<HashMap<String, ActivePrompt>>,
    /// Pending server->client permission requests, keyed by session and request
    /// id. One lock keeps registration and cancellation atomic.
    permissions: Mutex<HashMap<String, HashMap<String, oneshot::Sender<ApprovalDecision>>>>,
    next_request_id: AtomicU64,
    approval_timeout: Duration,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl AcpServer {
    pub fn new(
        app: AppClient,
        workspace: impl Into<PathBuf>,
        writer: Box<dyn AsyncWrite + Unpin + Send>,
    ) -> Self {
        let workspace = workspace.into();
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        Self {
            app,
            workspace,
            writer: Mutex::new(writer),
            initialized: AtomicBool::new(false),
            active: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(0),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn app(&self) -> &AppClient {
        &self.app
    }

    /// Serves newline-delimited ACP JSON-RPC until the client closes stdin.
    pub async fn serve_io<R>(self: Arc<Self>, reader: R) -> io::Result<()>
    where
        R: AsyncRead + Unpin,
    {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = read_bounded_line(&mut reader, &mut line, MAX_MESSAGE_BYTES).await?;
            match read {
                BoundedRead::Eof => break,
                BoundedRead::TooLarge => {
                    self.send_frame(json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": {"code": -32700, "message": "message exceeds size limit"},
                    }))
                    .await;
                }
                BoundedRead::Line => {
                    let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                        self.send_frame(json!({
                            "jsonrpc": "2.0",
                            "id": Value::Null,
                            "error": {"code": -32700, "message": "parse error"},
                        }))
                        .await;
                        continue;
                    };
                    self.clone().handle_message(message).await;
                }
            }
        }
        // Cancel runtime work, then let in-flight request handlers finish so a
        // response for the last frame is not lost at shutdown.
        let server = self.clone();
        server.shutdown().await;
        self.drain_tasks().await;
        Ok(())
    }

    async fn drain_tasks(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for task in tasks {
                let _ = task.await;
            }
        })
        .await;
    }

    async fn shutdown(self: Arc<Self>) {
        let active = self.active.lock().await.clone();
        for (session_id, prompt) in active {
            let key = new_idempotency_key("acp-shutdown");
            let _ = self
                .app
                .cancel_run(&key, &prompt.run_id, Some("disconnect"))
                .await;
            self.cancel_pending_permissions(&session_id).await;
        }
    }

    /// Handles one decoded JSON-RPC message or batch.
    pub async fn handle_message(self: Arc<Self>, message: Value) {
        match message {
            Value::Array(batch) => {
                if batch.is_empty() {
                    self.send_frame(protocol_error(Value::Null, -32600, "invalid request"))
                        .await;
                    return;
                }
                for item in batch {
                    self.clone().handle_single(item).await;
                }
            }
            other => self.handle_single(other).await,
        }
    }

    async fn handle_single(self: Arc<Self>, message: Value) {
        let Some(object) = message.as_object() else {
            self.send_frame(protocol_error(Value::Null, -32600, "invalid request"))
                .await;
            return;
        };
        let id = object.get("id").cloned();
        let method = object.get("method").and_then(Value::as_str);
        match (id, method) {
            (Some(id), None) => self.handle_client_response(id, object).await,
            (Some(id), Some(method)) => {
                let method = method.to_owned();
                let params = object.get("params").cloned().unwrap_or(Value::Null);
                if !valid_id(&id) || object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                    self.send_frame(protocol_error(Value::Null, -32600, "invalid request"))
                        .await;
                    return;
                }
                let server = self.clone();
                let task = tokio::spawn(async move {
                    let result = server.dispatch(&method, params).await;
                    match result {
                        Ok(result) => {
                            server
                                .send_frame(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                                .await
                        }
                        Err(error) => server.send_frame(error.to_json(id)).await,
                    }
                });
                let mut tasks = self.tasks.lock().await;
                tasks.retain(|task| !task.is_finished());
                tasks.push(task);
            }
            (None, Some("session/cancel")) => {
                let params = object.get("params").cloned().unwrap_or(Value::Null);
                if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                    self.cancel_session(session_id).await;
                }
            }
            _ => {}
        }
    }

    async fn dispatch(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        let params = normalize_params(params)?;
        match method {
            "initialize" => self.initialize(&params),
            _ if !self.initialized.load(Ordering::SeqCst) => Err(AcpError::new(
                -32600,
                "initialize must be called before session methods",
            )),
            "session/new" => self.new_session(&params).await,
            "session/load" => self.load_session(&params).await,
            "session/prompt" => self.prompt(&params).await,
            _ => Err(AcpError::new(-32601, "method not found")),
        }
    }

    fn initialize(&self, params: &serde_json::Map<String, Value>) -> Result<Value, AcpError> {
        if self.initialized.swap(true, Ordering::SeqCst) {
            return Err(AcpError::new(-32600, "initialize may only be called once"));
        }
        match params.get("protocolVersion") {
            // The client advertises its latest supported version; the agent
            // answers with the pinned ACP v1 version it implements and the
            // client decides whether it can continue.
            Some(Value::Number(number)) if number.as_u64().is_some_and(|value| value >= 1) => {}
            Some(_) => {
                self.initialized.store(false, Ordering::SeqCst);
                return Err(AcpError::invalid_params(
                    "protocolVersion must be a positive integer",
                ));
            }
            None => {
                self.initialized.store(false, Ordering::SeqCst);
                return Err(AcpError::invalid_params("protocolVersion is required"));
            }
        }
        if let Some(capabilities) = params.get("clientCapabilities")
            && !capabilities.is_object()
        {
            self.initialized.store(false, Ordering::SeqCst);
            return Err(AcpError::invalid_params(
                "clientCapabilities must be an object",
            ));
        }
        Ok(json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false,
                },
                "mcpCapabilities": {"http": false, "sse": false},
            },
            "authMethods": [],
            "agentInfo": {
                "name": "cool-ai-harness",
                "title": "Cool AI Harness",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }))
    }

    async fn new_session(
        &self,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, AcpError> {
        self.validate_cwd(params.get("cwd"))?;
        reject_unsupported_roots_and_mcp(params)?;
        let key = new_idempotency_key("acp-session");
        let session_id = self
            .app
            .create_session(&key, Some("ACP session"), None)
            .await
            .map_err(internal_error)?;
        Ok(json!({"sessionId": session_id}))
    }

    async fn load_session(
        &self,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, AcpError> {
        let session_id = required_string(params, "sessionId")?;
        self.validate_cwd(params.get("cwd"))?;
        reject_unsupported_roots_and_mcp(params)?;
        self.app
            .load_session(&session_id)
            .await
            .map_err(|_| AcpError::session_not_found())?;
        let history = self
            .app
            .session_history(&session_id, 200)
            .await
            .map_err(internal_error)?;
        for item in history.items {
            match item.role.as_str() {
                "user" => {
                    let text = item.content.unwrap_or_default();
                    self.notify(&session_id, crate::projection::message_chunk(&text, "user"))
                        .await;
                }
                "assistant" => {
                    if let Some(reasoning) = item.reasoning {
                        self.notify(&session_id, crate::projection::thought_chunk(&reasoning))
                            .await;
                    }
                    if let Some(content) = item.content
                        && !content.is_empty()
                    {
                        self.notify(
                            &session_id,
                            crate::projection::message_chunk(&content, "agent"),
                        )
                        .await;
                    }
                    for call in item.tool_calls {
                        let update = json!({
                            "sessionUpdate": "tool_call",
                            "toolCallId": call.call_id,
                            "title": call.name,
                            "kind": crate::projection::tool_kind(&call.name),
                            "status": "pending",
                            "rawInput": call.arguments,
                        });
                        self.notify(&session_id, update).await;
                    }
                }
                "tool" => {
                    let update = json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": item.tool_call_id.unwrap_or_default(),
                        "status": "completed",
                        "rawOutput": item.content,
                    });
                    self.notify(&session_id, update).await;
                }
                _ => {}
            }
        }
        let mut result = json!({});
        if let Some(title) = history_title(&self.app, &session_id).await {
            result["_meta"] = json!({"io.github.luckystrker.cool/title": title});
        }
        Ok(result)
    }

    async fn prompt(&self, params: &serde_json::Map<String, Value>) -> Result<Value, AcpError> {
        let session_id = required_string(params, "sessionId")?;
        let text = prompt_text(params.get("prompt"))?;
        if self.active.lock().await.contains_key(&session_id) {
            return Err(AcpError::new(
                -32600,
                "a prompt is already active for this session",
            ));
        }
        let mut events = self.app.subscribe();
        let key = new_idempotency_key("acp-prompt");
        let accepted = self
            .app
            .prompt(&key, &session_id, &text, None)
            .await
            .map_err(|error| match error {
                cool_app_server::ClientError::Protocol(protocol) => {
                    AcpError::new(-32000, protocol.message)
                }
                other => internal_error(other),
            })?;
        let run_id = accepted.run_id;
        self.active.lock().await.insert(
            session_id.clone(),
            ActivePrompt {
                run_id: run_id.clone(),
            },
        );
        let mut projection = AcpProjection::default();
        let mut last_seq = 0_u64;
        let stop_reason = loop {
            let envelope = match events.recv().await {
                Ok(envelope) => envelope,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Recover the dropped range from the durable run log instead
                    // of waiting for a terminal event that may already be gone.
                    if let Some(reason) = self
                        .catch_up_run(&session_id, &run_id, &mut last_seq, &mut projection)
                        .await
                    {
                        break reason;
                    }
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break "refusal".to_owned();
                }
            };
            if envelope.run_id != run_id {
                continue;
            }
            if envelope.seq <= last_seq {
                // `Lagged` repositions the receiver to the oldest retained slot;
                // catch-up already projected everything up to `last_seq`.
                continue;
            }
            last_seq = envelope.seq;
            if let Some(reason) = self
                .consume_event(&session_id, &mut projection, &envelope)
                .await
            {
                break reason;
            }
        };
        self.active.lock().await.remove(&session_id);
        self.cancel_pending_permissions(&session_id).await;
        Ok(json!({"stopReason": stop_reason}))
    }

    /// Projects one canonical envelope to ACP and returns a terminal reason.
    async fn consume_event(
        &self,
        session_id: &str,
        projection: &mut AcpProjection,
        envelope: &cool_protocol::EventEnvelope,
    ) -> Option<String> {
        for update in projection.adapt(envelope) {
            self.notify(session_id, update).await;
        }
        if let cool_protocol::CanonicalEvent::ToolApprovalRequired(approval) = &envelope.event {
            self.resolve_permission(
                session_id,
                &approval.approval_id,
                approval.revision,
                &approval.call_id,
                &approval.name,
                &serde_json::to_value(&approval.arguments).unwrap_or(Value::Null),
            )
            .await;
        }
        match &envelope.event {
            cool_protocol::CanonicalEvent::RunCompleted(terminal) => {
                Some(stop_reason(&terminal.reason).to_owned())
            }
            cool_protocol::CanonicalEvent::RunFailed(_) => Some("refusal".to_owned()),
            cool_protocol::CanonicalEvent::RunCancelled(_) => Some("cancelled".to_owned()),
            _ => None,
        }
    }

    async fn catch_up_run(
        &self,
        session_id: &str,
        run_id: &str,
        last_seq: &mut u64,
        projection: &mut AcpProjection,
    ) -> Option<String> {
        let mut after = Some(*last_seq);
        loop {
            let Ok(page) = self.app.run_events(run_id, after, 100).await else {
                return None;
            };
            for envelope in &page.events {
                *last_seq = envelope.seq;
                if let Some(reason) = self.consume_event(session_id, projection, envelope).await {
                    return Some(reason);
                }
            }
            if !page.has_more {
                return None;
            }
            after = page
                .next_cursor
                .as_ref()
                .and_then(|cursor| cursor.after_seq);
            if after.is_some() {
                continue;
            }
            return None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_permission(
        &self,
        session_id: &str,
        approval_id: &str,
        revision: u64,
        call_id: &str,
        name: &str,
        arguments: &Value,
    ) {
        let request_id = format!(
            "cool-acp-{}",
            self.next_request_id.fetch_add(1, Ordering::SeqCst) + 1
        );
        let (sender, receiver) = oneshot::channel();
        self.permissions
            .lock()
            .await
            .entry(session_id.to_owned())
            .or_default()
            .insert(request_id.clone(), sender);
        let tool_call = AcpProjection::default().permission_tool_call(call_id, name, arguments);
        self.send_frame(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session_id,
                "toolCall": tool_call,
                "options": [
                    {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"},
                ],
            },
        }))
        .await;
        let decision = match tokio::time::timeout(self.approval_timeout, receiver).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) | Err(_) => ApprovalDecision::Denied,
        };
        let mut permissions = self.permissions.lock().await;
        if let Some(pending) = permissions.get_mut(session_id) {
            pending.remove(&request_id);
            if pending.is_empty() {
                permissions.remove(session_id);
            }
        }
        drop(permissions);
        let key = format!("acp-approval-{approval_id}-{revision}");
        let _ = self
            .app
            .resolve_approval(&key, approval_id, revision, decision)
            .await;
    }

    async fn handle_client_response(&self, id: Value, object: &serde_json::Map<String, Value>) {
        let Some(request_id) = id.as_str() else {
            return;
        };
        let sender = {
            let mut permissions = self.permissions.lock().await;
            let session = permissions.iter().find_map(|(session, pending)| {
                pending.contains_key(request_id).then(|| session.clone())
            });
            session.and_then(|session| {
                permissions
                    .get_mut(&session)
                    .and_then(|pending| pending.remove(request_id))
            })
        };
        let Some(sender) = sender else {
            return;
        };
        let malformed = object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || object.contains_key("result") == object.contains_key("error");
        if malformed {
            let _ = sender.send(ApprovalDecision::Denied);
            return;
        }
        let selected = object
            .get("result")
            .and_then(|result| result.get("outcome"))
            .map(|outcome| {
                (
                    outcome.get("outcome").and_then(Value::as_str),
                    outcome.get("optionId").and_then(Value::as_str),
                )
            });
        let decision = match selected {
            Some((Some("selected"), Some("allow_once"))) => ApprovalDecision::Approved,
            _ => ApprovalDecision::Denied,
        };
        let _ = sender.send(decision);
    }

    async fn cancel_session(&self, session_id: &str) {
        let Some(prompt) = self.active.lock().await.get(session_id).cloned() else {
            return;
        };
        let key = new_idempotency_key("acp-cancel");
        let _ = self
            .app
            .cancel_run(&key, &prompt.run_id, Some("client"))
            .await;
        self.cancel_pending_permissions(session_id).await;
    }

    async fn cancel_pending_permissions(&self, session_id: &str) {
        let pending = self
            .permissions
            .lock()
            .await
            .remove(session_id)
            .unwrap_or_default();
        for sender in pending.into_values() {
            let _ = sender.send(ApprovalDecision::Denied);
        }
    }

    fn validate_cwd(&self, value: Option<&Value>) -> Result<(), AcpError> {
        let Some(raw) = value.and_then(Value::as_str) else {
            return Err(AcpError::invalid_params(
                "cwd must be a non-empty absolute path",
            ));
        };
        let path = Path::new(raw);
        if !path.is_absolute() || !path.is_dir() {
            return Err(AcpError::invalid_params(
                "cwd must be an absolute directory",
            ));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| AcpError::invalid_params("cwd directory does not exist"))?;
        if canonical != self.workspace {
            return Err(AcpError::invalid_params(
                "cwd must match the server working directory",
            ));
        }
        Ok(())
    }

    async fn notify(&self, session_id: &str, update: Value) {
        self.send_frame(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": session_id, "update": update},
        }))
        .await;
    }

    pub async fn send_frame(&self, frame: Value) {
        let mut encoded = match serde_json::to_vec(&frame) {
            Ok(encoded) => encoded,
            Err(_) => return,
        };
        encoded.push(b'\n');
        let mut writer = self.writer.lock().await;
        let _ = writer.write_all(&encoded).await;
        let _ = writer.flush().await;
    }
}

async fn history_title(app: &AppClient, session_id: &str) -> Option<String> {
    app.list_sessions(None, 100)
        .await
        .ok()?
        .sessions
        .into_iter()
        .find(|session| session.session_id == session_id)?
        .title
}

fn normalize_params(params: Value) -> Result<serde_json::Map<String, Value>, AcpError> {
    match params {
        Value::Null => Ok(serde_json::Map::new()),
        Value::Object(map) => Ok(map),
        _ => Err(AcpError::invalid_params("params must be an object")),
    }
}

fn required_string(params: &serde_json::Map<String, Value>, key: &str) -> Result<String, AcpError> {
    match params.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_owned()),
        _ => Err(AcpError::invalid_params(format!(
            "{key} must be a non-empty string"
        ))),
    }
}

fn reject_unsupported_roots_and_mcp(
    params: &serde_json::Map<String, Value>,
) -> Result<(), AcpError> {
    if let Some(additional) = params.get("additionalDirectories") {
        let values = additional
            .as_array()
            .ok_or_else(|| AcpError::invalid_params("additionalDirectories must be an array"))?;
        if !values.is_empty() {
            return Err(AcpError::invalid_params(
                "additionalDirectories are not supported by this adapter",
            ));
        }
    }
    if let Some(mcp) = params.get("mcpServers") {
        let values = mcp
            .as_array()
            .ok_or_else(|| AcpError::invalid_params("mcpServers must be an array"))?;
        if !values.is_empty() {
            return Err(AcpError::invalid_params(
                "client-supplied MCP servers are not supported by this adapter",
            ));
        }
    }
    Ok(())
}

pub fn prompt_text(value: Option<&Value>) -> Result<String, AcpError> {
    let Some(blocks) = value.and_then(Value::as_array) else {
        return Err(AcpError::invalid_params("prompt must be a non-empty array"));
    };
    if blocks.is_empty() {
        return Err(AcpError::invalid_params("prompt must be a non-empty array"));
    }
    let mut parts = Vec::new();
    for block in blocks {
        let Some(object) = block.as_object() else {
            return Err(AcpError::invalid_params("prompt blocks must be objects"));
        };
        match object.get("type").and_then(Value::as_str) {
            Some("text") => match object.get("text").and_then(Value::as_str) {
                Some(text) => parts.push(text.to_owned()),
                None => {
                    return Err(AcpError::invalid_params(
                        "text prompt blocks require a string text field",
                    ));
                }
            },
            Some("resource_link") => {
                let uri = object.get("uri").and_then(Value::as_str);
                let name = object.get("name").and_then(Value::as_str);
                match (uri, name) {
                    (Some(uri), Some(name)) if !uri.is_empty() && !name.is_empty() => {
                        // Resource links stay references; the adapter never
                        // performs an implicit fetch around capability checks.
                        parts.push(format!("[ACP resource: {name}]({uri})"));
                    }
                    _ => {
                        return Err(AcpError::invalid_params(
                            "resource_link prompt blocks require non-empty uri and name fields",
                        ));
                    }
                }
            }
            other => {
                return Err(AcpError::invalid_params(format!(
                    "unsupported ACP prompt block type: {other:?}"
                )));
            }
        }
    }
    let combined = parts.join("\n");
    if combined.trim().is_empty() {
        return Err(AcpError::invalid_params("prompt text must not be empty"));
    }
    Ok(combined)
}

pub fn stop_reason(reason: &str) -> &'static str {
    match reason {
        "cancelled" | "canceled" => "cancelled",
        "max_tokens" | "length" => "max_tokens",
        "max_iterations" | "max_turns" => "max_turn_requests",
        "error" | "failed" | "denied" => "refusal",
        _ => "end_turn",
    }
}

fn internal_error(error: impl std::fmt::Display) -> AcpError {
    let mut mapped = AcpError::new(-32603, "internal error");
    mapped.data = Some(json!({"type": error.to_string()}));
    mapped
}

fn protocol_error(id: Value, code: i32, message: &str) -> Value {
    AcpError::new(code, message).to_json(id)
}

fn valid_id(value: &Value) -> bool {
    matches!(value, Value::String(_)) || value.as_i64().is_some()
}

enum BoundedRead {
    Line,
    TooLarge,
    Eof,
}

async fn read_bounded_line<R>(
    reader: &mut BufReader<R>,
    line: &mut Vec<u8>,
    limit: usize,
) -> io::Result<BoundedRead>
where
    R: AsyncRead + Unpin,
{
    let mut overflow = false;
    let mut saw_any = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if overflow {
                return Ok(BoundedRead::TooLarge);
            }
            return Ok(if saw_any {
                BoundedRead::Line
            } else {
                BoundedRead::Eof
            });
        }
        saw_any = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if !overflow {
            let payload = if newline.is_some() {
                consumed - 1
            } else {
                consumed
            };
            if line.len() + payload > limit {
                overflow = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..payload]);
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(if overflow {
                BoundedRead::TooLarge
            } else {
                BoundedRead::Line
            });
        }
    }
}
