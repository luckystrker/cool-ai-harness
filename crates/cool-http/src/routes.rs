//! HTTP routes for the Web facade.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware;
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use cool_app_server::{AppClient, ClientError};
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
const DEFAULT_PAGE_LIMIT: u16 = 128;
const MAX_PAGE_LIMIT: u16 = 256;
const LIVE_IDLE: Duration = Duration::from_secs(15);

pub(crate) fn router(state: Arc<FacadeState>) -> Router {
    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/rpc", post(rpc))
        .route("/api/events", get(events));
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
        phase: Phase::Catchup,
        backlog: VecDeque::new(),
        finished: false,
    };
    stream::unfold(initial, |mut state| async move {
        loop {
            match state.phase {
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
                            if !page.has_more && state.backlog.is_empty() {
                                state.phase = Phase::Live;
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
