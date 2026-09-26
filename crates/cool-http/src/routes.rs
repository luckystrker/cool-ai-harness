//! HTTP routes for the Web facade.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware;
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use cool_app_server::{AppClient, BlobError, ClientError};
use cool_protocol::{
    CanonicalEvent, EventEnvelope, JsonRpcV2, ProtocolError, RpcFailure, RpcId, RpcRequest,
    RpcSuccess, ServerFrame, StreamEnd, StreamKeepalive,
};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::timeout;
use tower_http::services::{ServeDir, ServeFile};

use crate::{ClientId, FacadeState};

/// Matches the App Protocol transport frame limit.
const MAX_BODY_BYTES: usize = 1_048_576;
/// Multipart upload cap: `BlobStore::MAX_UPLOAD_BYTES` + form overhead.
const MAX_UPLOAD_BODY_BYTES: usize = 51_000_000;
const DEFAULT_PAGE_LIMIT: u16 = 128;
const MAX_PAGE_LIMIT: u16 = 256;
const LIVE_IDLE: Duration = Duration::from_secs(15);

pub(crate) fn router(state: Arc<FacadeState>) -> Router {
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/rpc", post(rpc))
        .route("/api/events", get(events))
        // Legacy binary/blob surface — too large for the JSON-RPC frame limit.
        .route(
            "/api/conversations/{conversation_id}/artifacts",
            post(upload_artifact).layer(DefaultBodyLimit::max(MAX_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/api/conversations/{conversation_id}/artifacts/{artifact_id}/download",
            get(download_artifact),
        )
        .route("/api/memory/export", get(memory_export))
        .route(
            "/api/research/{research_run_id}/export",
            get(research_export),
        );
    let app = match &state.assets {
        Some(directory) => api.fallback_service(spa(directory)),
        None => api.fallback(placeholder),
    };
    app.layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::client_identity,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::authorize,
        ))
        .with_state(state)
}

fn spa(directory: &Path) -> ServeDir<ServeFile> {
    ServeDir::new(directory).fallback(ServeFile::new(directory.join("index.html")))
}

// --- blob routes -------------------------------------------------------------

fn blob_error_response(error: BlobError) -> Response {
    let (status, code) = match &error {
        BlobError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
        BlobError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_input"),
        BlobError::Store(_) => (StatusCode::INTERNAL_SERVER_ERROR, "store_error"),
        BlobError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "not_found")
        }
        BlobError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error"),
        BlobError::WorkerUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "worker_unavailable"),
    };
    (
        status,
        Json(json!({"coolCode": code, "message": error.to_string()})),
    )
        .into_response()
}

fn attachment_response(body: Vec<u8>, media_type: &'static str, filename: &str) -> Response {
    let mut response = (StatusCode::OK, body).into_response();
    if let Ok(value) = HeaderValue::from_str(media_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// `POST /api/conversations/{id}/artifacts` — Python `upload_artifact` parity.
#[derive(Debug, Deserialize)]
struct UploadQuery {
    run_id: Option<i64>,
    kind: Option<String>,
}

async fn upload_artifact(
    State(state): State<Arc<FacadeState>>,
    UrlPath(conversation_id): UrlPath<i64>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Response {
    let Some(blobs) = state.blobs.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"coolCode": "blob_store_unavailable"})),
        )
            .into_response();
    };
    let mut filename = None;
    let mut content = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            filename = field.file_name().map(str::to_owned);
            match field.bytes().await {
                Ok(bytes) => content = Some(bytes),
                Err(error) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(
                            json!({"coolCode": "invalid_multipart", "message": error.to_string()}),
                        ),
                    )
                        .into_response();
                }
            }
        }
    }
    let Some(content) = content else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"coolCode": "invalid_multipart", "message": "missing 'file' field"})),
        )
            .into_response();
    };
    let filename = filename.unwrap_or_else(|| "upload.bin".to_owned());
    match blobs.upload(
        &cool_app_server::local_actor_id(),
        conversation_id,
        &filename,
        &content,
        query.run_id,
        query.kind.as_deref(),
    ) {
        Ok(artifact) => Json(json!({
            // ArtifactUploadResponse (snake_case ArtifactOut), Python parity.
            "artifact": {
                "id": artifact.id,
                "conversation_id": artifact.conversation_id,
                "run_id": artifact.run_id,
                "tool_call_id": artifact.tool_call_id,
                "filename": artifact.filename,
                "media_type": artifact.media_type,
                "kind": artifact.kind,
                "size_bytes": artifact.size_bytes,
                "sha256": artifact.sha256,
                "version": artifact.version,
                "parent_id": artifact.parent_id,
                "metadata_": artifact.metadata,
                "created_at": artifact.created_at,
                "updated_at": artifact.updated_at,
            },
            "message": "uploaded",
        }))
        .into_response(),
        Err(error) => blob_error_response(error),
    }
}

/// `GET /api/conversations/{id}/artifacts/{id}/download` — FileResponse parity.
async fn download_artifact(
    State(state): State<Arc<FacadeState>>,
    UrlPath((conversation_id, artifact_id)): UrlPath<(i64, i64)>,
) -> Response {
    let Some(blobs) = state.blobs.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"coolCode": "blob_store_unavailable"})),
        )
            .into_response();
    };
    let file = match blobs.open_artifact(
        &cool_app_server::local_actor_id(),
        conversation_id,
        artifact_id,
    ) {
        Ok(file) => file,
        Err(error) => return blob_error_response(error),
    };
    match tokio::fs::read(&file.path).await {
        Ok(bytes) => {
            let mut response = (StatusCode::OK, bytes).into_response();
            if let Ok(value) = HeaderValue::from_str(&file.artifact.media_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            if let Ok(value) = HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                file.artifact.filename
            )) {
                response
                    .headers_mut()
                    .insert(header::CONTENT_DISPOSITION, value);
            }
            response
        }
        Err(error) => blob_error_response(BlobError::Io(error)),
    }
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    #[serde(default = "default_json")]
    format: String,
    #[serde(default)]
    include_archived: bool,
}

fn default_json() -> String {
    "json".to_owned()
}

/// `GET /api/memory/export` — JSON/markdown memory dump, attachment response.
async fn memory_export(
    State(state): State<Arc<FacadeState>>,
    Query(query): Query<ExportQuery>,
) -> Response {
    let Some(blobs) = state.blobs.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"coolCode": "blob_store_unavailable"})),
        )
            .into_response();
    };
    match blobs.export_memories(
        &cool_app_server::local_actor_id(),
        &query.format.to_lowercase(),
        query.include_archived,
    ) {
        Ok((body, media_type, filename)) => {
            attachment_response(body.into_bytes(), media_type, &filename)
        }
        Err(BlobError::Invalid(message)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"coolCode": "invalid_input", "message": message})),
        )
            .into_response(),
        Err(error) => blob_error_response(error),
    }
}

#[derive(Debug, Deserialize)]
struct ResearchExportQuery {
    #[serde(default = "default_md")]
    format: String,
}

fn default_md() -> String {
    "md".to_owned()
}

/// `GET /api/research/{id}/export` — md/html here; pdf/docx on the optional
/// Python worker lane.
async fn research_export(
    State(state): State<Arc<FacadeState>>,
    UrlPath(run_id): UrlPath<i64>,
    Query(query): Query<ResearchExportQuery>,
) -> Response {
    let Some(blobs) = state.blobs.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"coolCode": "blob_store_unavailable"})),
        )
            .into_response();
    };
    match blobs.export_research(
        &cool_app_server::local_actor_id(),
        run_id,
        &query.format.to_lowercase(),
    ) {
        Ok((body, media_type, filename)) => attachment_response(body, media_type, &filename),
        Err(error) => blob_error_response(error),
    }
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "runtime": "rust-trusted-core",
        "protocolVersion": 1,
        "phase": "M11",
        "server": "cool-http",
        "serverVersion": env!("CARGO_PKG_VERSION"),
        "capabilities": cool_app_server::capabilities(),
    }))
}

/// JSON-RPC command endpoint. The request is the canonical `RpcRequest`; the
/// response is the canonical `ServerFrame`, so a client never sees an
/// HTTP-specific business model.
async fn rpc(
    State(state): State<Arc<FacadeState>>,
    Extension(identity): Extension<ClientId>,
    body: Bytes,
) -> Response {
    let request = match serde_json::from_slice::<RpcRequest>(&body) {
        Ok(request) => request,
        Err(error) => {
            return Json(ServerFrame::Failure(RpcFailure {
                jsonrpc: JsonRpcV2::VALUE,
                id: RpcId::Null,
                error: protocol_error(-32700, "parse_error", &error.to_string()),
            }))
            .into_response();
        }
    };
    let client = match state.pool.get(&identity.value).await {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "coolCode": "protocol_connection_unavailable",
                    "message": error.to_string(),
                    "retryable": true
                })),
            )
                .into_response();
        }
    };
    let id = request.id;
    match client.request(request.params.command).await {
        Ok(payload) => Json(ServerFrame::Success(RpcSuccess {
            jsonrpc: JsonRpcV2::VALUE,
            id,
            result: payload,
        }))
        .into_response(),
        Err(ClientError::Protocol(error)) => Json(ServerFrame::Failure(RpcFailure {
            jsonrpc: JsonRpcV2::VALUE,
            id,
            error,
        }))
        .into_response(),
        Err(error) => Json(ServerFrame::Failure(RpcFailure {
            jsonrpc: JsonRpcV2::VALUE,
            id,
            error: protocol_error(-32000, "transport_closed", &error.to_string()),
        }))
        .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default, alias = "runId")]
    run_id: Option<String>,
    #[serde(default, alias = "afterSeq")]
    after_seq: Option<u64>,
    #[serde(default)]
    limit: Option<u16>,
}

async fn events(
    State(state): State<Arc<FacadeState>>,
    Extension(identity): Extension<ClientId>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    let Some(run_id) = query.run_id.filter(|value| !value.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"coolCode": "missing_run_id", "message": "run_id is required"})),
        )
            .into_response();
    };
    // An unauthenticated request that would create a brand-new identity must
    // not spawn a durable protocol connection: a cross-site no-cors GET sends
    // no Origin and no SameSite=Strict cookie, so this is the only thing
    // standing between it and pool exhaustion.
    if identity.minted && !state.auth.token_configured() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "coolCode": "client_identity_required",
                "message": "the event stream requires an established client identity",
                "retryable": false
            })),
        )
            .into_response();
    }
    let client = match state.pool.get(&identity.value).await {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "coolCode": "protocol_connection_unavailable",
                    "message": error.to_string(),
                    "retryable": true
                })),
            )
                .into_response();
        }
    };
    // Native `EventSource` reconnect sends `Last-Event-ID`; prefer it so a
    // dropped browser stream resumes without gaps or duplicates.
    let after_seq = last_event_id(&headers).or(query.after_seq);
    let max_limit = state.pool.max_event_page_limit().min(MAX_PAGE_LIMIT);
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT.min(max_limit))
        .clamp(1, max_limit);
    let stream = event_stream(client, run_id, after_seq, limit);
    Sse::new(stream).into_response()
}

#[derive(Debug)]
enum Phase {
    /// Register this browser connection as a `run.subscribe` follower of the
    /// requested run before replaying its durable backlog.
    Subscribe,
    Catchup,
    Live,
    End,
}

struct StreamState {
    client: AppClient,
    live: tokio::sync::broadcast::Receiver<EventEnvelope>,
    run_id: String,
    cursor: Option<u64>,
    last_seq: u64,
    limit: u16,
    phase: Phase,
    backlog: VecDeque<EventEnvelope>,
    /// The requested run was already terminal when `run.subscribe` answered, so
    /// the stream must end after the durable catch-up even when the client's
    /// cursor is past the terminal seq (no live event will ever arrive).
    already_terminal: bool,
    finished: bool,
}

fn event_stream(
    client: AppClient,
    run_id: String,
    after_seq: Option<u64>,
    limit: u16,
) -> impl futures_util::Stream<Item = Result<Event, Infallible>> {
    // Subscribe before catch-up so live events during the backlog are buffered
    // rather than lost; the seq filter then removes any overlap.
    let live = client.subscribe();
    let initial = StreamState {
        client,
        live,
        run_id,
        cursor: after_seq,
        last_seq: after_seq.unwrap_or(0),
        limit,
        phase: Phase::Subscribe,
        backlog: VecDeque::new(),
        already_terminal: false,
        finished: false,
    };
    stream::unfold(initial, |mut state| async move {
        loop {
            match state.phase {
                Phase::Subscribe => {
                    // A browser on a second identity can only receive a run's
                    // live events after the runtime registers it as a
                    // subscriber; `run.subscribe` also reports whether the run
                    // already terminated so the stream cannot hang waiting for
                    // an event that was already persisted.
                    match state.client.run_subscribe(&state.run_id).await {
                        Ok(result) => {
                            state.already_terminal = result.terminal;
                            state.phase = Phase::Catchup;
                        }
                        Err(ClientError::Protocol(error)) => {
                            state.finished = true;
                            state.phase = Phase::End;
                            return Some((Ok(error_event(&error)), state));
                        }
                        Err(error) => {
                            state.finished = true;
                            state.phase = Phase::End;
                            return Some((
                                Ok(error_event(&protocol_error(
                                    -32000,
                                    "transport_closed",
                                    &error.to_string(),
                                ))),
                                state,
                            ));
                        }
                    }
                }
                Phase::Catchup => {
                    if let Some(envelope) = state.backlog.pop_front() {
                        state.last_seq = state.last_seq.max(envelope.seq);
                        state.cursor = Some(state.last_seq);
                        if is_terminal(&envelope.event) {
                            state.phase = Phase::End;
                        }
                        return Some((Ok(event_from(&envelope)), state));
                    }
                    match state
                        .client
                        .run_events(&state.run_id, state.cursor, state.limit)
                        .await
                    {
                        Ok(page) => {
                            state.backlog.extend(page.events);
                            // Drain the whole backlog before going live; only
                            // switch when this page is final and fully queued.
                            // An already-terminal run ends after the backlog
                            // instead of waiting for a fan-out that will never
                            // come.
                            if !page.has_more && state.backlog.is_empty() {
                                state.phase = if state.already_terminal {
                                    Phase::End
                                } else {
                                    Phase::Live
                                };
                            }
                        }
                        Err(ClientError::Protocol(error)) => {
                            state.finished = true;
                            state.phase = Phase::End;
                            return Some((Ok(error_event(&error)), state));
                        }
                        Err(error) => {
                            state.finished = true;
                            state.phase = Phase::End;
                            return Some((
                                Ok(error_event(&protocol_error(
                                    -32000,
                                    "transport_closed",
                                    &error.to_string(),
                                ))),
                                state,
                            ));
                        }
                    }
                }
                Phase::Live => match timeout(LIVE_IDLE, state.live.recv()).await {
                    Ok(Ok(envelope)) => {
                        if envelope.run_id == state.run_id && envelope.seq > state.last_seq {
                            state.last_seq = envelope.seq;
                            state.cursor = Some(envelope.seq);
                            if is_terminal(&envelope.event) {
                                state.phase = Phase::End;
                            }
                            return Some((Ok(event_from(&envelope)), state));
                        }
                    }
                    Ok(Err(RecvError::Lagged(_))) => state.phase = Phase::Catchup,
                    Ok(Err(RecvError::Closed)) => state.phase = Phase::End,
                    Err(_elapsed) => {
                        return Some((Ok(keepalive_event(&state.run_id, state.last_seq)), state));
                    }
                },
                Phase::End => {
                    if state.finished {
                        return None;
                    }
                    state.finished = true;
                    return Some((Ok(end_event(&state.run_id, "terminal")), state));
                }
            }
        }
    })
}

fn event_from(envelope: &EventEnvelope) -> Event {
    Event::default()
        .id(envelope.seq.to_string())
        .event("run.event")
        .data(serde_json::to_string(envelope).expect("event envelope serializes"))
}

fn keepalive_event(run_id: &str, last_seq: u64) -> Event {
    // Canonical `StreamFrame::Keepalive` payload.
    let payload = StreamKeepalive {
        run_id: run_id.to_owned(),
        last_seq,
    };
    Event::default()
        .event("keepalive")
        .data(serde_json::to_string(&payload).expect("keepalive serializes"))
}

fn end_event(run_id: &str, reason: &str) -> Event {
    // Canonical `StreamFrame::End` payload.
    let payload = StreamEnd {
        run_id: run_id.to_owned(),
        reason: reason.to_owned(),
    };
    Event::default()
        .event("end")
        .data(serde_json::to_string(&payload).expect("stream end serializes"))
}

fn error_event(error: &ProtocolError) -> Event {
    Event::default().event("error").data(
        json!({
            "coolCode": error.cool_code,
            "message": error.message,
            "retryable": error.retryable,
        })
        .to_string(),
    )
}

fn protocol_error(rpc_code: i32, cool_code: &str, message: &str) -> ProtocolError {
    ProtocolError {
        rpc_code,
        cool_code: cool_code.to_owned(),
        message: message.to_owned(),
        retryable: false,
        safe_details: Default::default(),
    }
}

fn is_terminal(event: &CanonicalEvent) -> bool {
    matches!(
        event,
        CanonicalEvent::RunCompleted(_)
            | CanonicalEvent::RunFailed(_)
            | CanonicalEvent::RunCancelled(_)
    )
}

fn last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

async fn placeholder(_uri: Uri) -> Response {
    Html(
        "<!doctype html><html><head><title>Cool</title></head>\
         <body style=\"font-family:system-ui;padding:2rem\">\
         <h1>Cool Rust runtime</h1>\
         <p>The React bundle is not configured. Build <code>frontend/</code> and start \
         <code>cool serve --assets frontend/dist</code>.</p>\
         <p><a href=\"/api/health\">/api/health</a></p></body></html>",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_stream_frame_payloads_serialize_as_expected() {
        let keepalive = StreamKeepalive {
            run_id: "run-1".to_owned(),
            last_seq: 7,
        };
        assert_eq!(
            serde_json::to_string(&keepalive).expect("serializes"),
            r#"{"runId":"run-1","lastSeq":7}"#
        );
        let end = StreamEnd {
            run_id: "run-1".to_owned(),
            reason: "terminal".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&end).expect("serializes"),
            r#"{"runId":"run-1","reason":"terminal"}"#
        );
    }

    #[test]
    fn terminal_events_are_recognized() {
        assert!(is_terminal(&CanonicalEvent::RunCompleted(
            cool_protocol::RunTerminal {
                reason: "stop".to_owned(),
                error_code: None,
            }
        )));
        assert!(!is_terminal(&CanonicalEvent::ContentDelta(
            cool_protocol::TextDelta {
                text: "x".to_owned(),
                channel: None,
            }
        )));
    }
}
