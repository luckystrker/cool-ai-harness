use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::core::{CoreOutcome, PromptRequest, SpikeCore};
use crate::framing::read_limited_line;
use crate::model::{Event, SpikeError, SpikeResult};

const EVENT_CHANNEL_CAPACITY: usize = 16;
const DELIVERY_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct PromptParams {
    session_id: String,
    idempotency_key: String,
    prompt: String,
    #[serde(default = "normal_mode")]
    mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct ApprovalParams {
    approval_id: String,
    expected_revision: i64,
    approved: bool,
    idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct RunParams {
    run_id: String,
    #[serde(default = "before_first_event")]
    after_seq: i64,
}

#[derive(Debug)]
struct ProtocolOutput {
    response: Value,
    notifications: Vec<Event>,
}

pub async fn serve_jsonl<R, W>(mut reader: R, mut writer: W, core: SpikeCore) -> SpikeResult<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let frame = match read_limited_line(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(SpikeError::FrameTooLarge) => {
                write_json_with_deadline(
                    &mut writer,
                    &error_response(
                        Value::Null,
                        -32030,
                        "frame_too_large",
                        false,
                        "request frame exceeds the configured limit",
                    ),
                )
                .await?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if frame.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let request = match serde_json::from_slice::<JsonRpcRequest>(&frame) {
            Ok(request) => request,
            Err(error) => {
                write_json_with_deadline(
                    &mut writer,
                    &error_response(
                        Value::Null,
                        -32700,
                        "parse_error",
                        false,
                        &error.to_string(),
                    ),
                )
                .await?;
                continue;
            }
        };

        let (event_sender, mut event_receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let mut handler = Box::pin(handle_request(&core, request, event_sender));
        let mut writer_error = None;
        let output = loop {
            tokio::select! {
                event = event_receiver.recv() => {
                    if let Some(event) = event
                        && writer_error.is_none()
                        && let Err(error) = deliver_event(&mut writer, &event).await
                    {
                        writer_error = Some(error);
                    }
                }
                output = &mut handler => break output,
            }
        };
        while let Ok(event) = event_receiver.try_recv() {
            if writer_error.is_none()
                && let Err(error) = deliver_event(&mut writer, &event).await
            {
                writer_error = Some(error);
            }
        }
        if writer_error.is_none() {
            for event in &output.notifications {
                if let Err(error) = deliver_event(&mut writer, event).await {
                    writer_error = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = writer_error {
            return Err(error);
        }
        write_json_with_deadline(&mut writer, &output.response).await?;
    }
    Ok(())
}

async fn handle_request(
    core: &SpikeCore,
    request: JsonRpcRequest,
    event_sender: mpsc::Sender<Event>,
) -> ProtocolOutput {
    if request.jsonrpc != "2.0" {
        return ProtocolOutput {
            response: error_response(
                request.id,
                -32600,
                "invalid_request",
                false,
                "jsonrpc must be 2.0",
            ),
            notifications: Vec::new(),
        };
    }
    let id = request.id;
    let result = match request.method.as_str() {
        "initialize" => Ok((
            json!({
                "protocolVersion": 1,
                "server": {"name": "cool-m0-spike", "production": false},
                "capabilities": {
                    "eventReplay": true,
                    "cursorCatchup": true,
                    "idempotentCommands": true,
                    "approvalRevision": true,
                    "streamingEvents": true
                }
            }),
            Vec::new(),
        )),
        "session.prompt" => match parse::<PromptParams>(request.params) {
            Ok(params) => core
                .prompt(
                    PromptRequest {
                        session_id: params.session_id,
                        actor: SPIKE_ACTOR.to_owned(),
                        idempotency_key: params.idempotency_key,
                        prompt: params.prompt,
                        mode: params.mode,
                    },
                    &event_sender,
                )
                .await
                .map(outcome_to_protocol),
            Err(error) => Err(error),
        },
        "approval.resolve" => match parse::<ApprovalParams>(request.params) {
            Ok(params) => core
                .resolve_approval(
                    SPIKE_ACTOR,
                    &params.approval_id,
                    params.expected_revision,
                    params.approved,
                    &params.idempotency_key,
                    &event_sender,
                )
                .await
                .map(outcome_to_protocol),
            Err(error) => Err(error),
        },
        "run.get" => parse::<RunParams>(request.params).and_then(|params| {
            let run = core.store().run(&params.run_id)?;
            Ok((json!({"run": run}), Vec::new()))
        }),
        "run.events" => parse::<RunParams>(request.params).and_then(|params| {
            let events = core
                .store()
                .list_events(&params.run_id, Some(params.after_seq))?;
            Ok((json!({"events": events}), Vec::new()))
        }),
        "run.catchup" => parse::<RunParams>(request.params).and_then(|params| {
            let events = core
                .store()
                .list_events(&params.run_id, Some(params.after_seq))?;
            let latest_seq = events.last().map(|event| event.seq);
            Ok((
                json!({"runId": params.run_id, "latestSeq": latest_seq}),
                events,
            ))
        }),
        _ => Err(SpikeError::MethodNotFound(request.method)),
    };

    match result {
        Ok((body, notifications)) => ProtocolOutput {
            response: json!({"jsonrpc": "2.0", "id": id, "result": body}),
            notifications,
        },
        Err(error) => ProtocolOutput {
            response: protocol_error(id, &error),
            notifications: Vec::new(),
        },
    }
}

fn outcome_to_protocol(outcome: CoreOutcome) -> (Value, Vec<Event>) {
    (
        json!({
            "runId": outcome.receipt.run_id,
            "status": outcome.receipt.status,
            "approvalId": outcome.receipt.approval_id,
            "approvalRevision": outcome.receipt.approval_revision
        }),
        Vec::new(),
    )
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> SpikeResult<T> {
    serde_json::from_value(value)
        .map_err(|error| SpikeError::Protocol(format!("invalid params: {error}")))
}

fn protocol_error(id: Value, error: &SpikeError) -> Value {
    let (code, cool_code, retryable, message) = match error {
        SpikeError::StaleApproval => (-32010, "stale_approval", false, error.to_string()),
        SpikeError::InvalidTransition { .. } => {
            (-32011, "invalid_transition", false, error.to_string())
        }
        SpikeError::IdempotencyConflict => {
            (-32012, "idempotency_conflict", false, error.to_string())
        }
        SpikeError::MethodNotFound(_) => (-32601, "method_not_found", false, error.to_string()),
        SpikeError::Protocol(_) => (-32602, "invalid_params", false, error.to_string()),
        SpikeError::Worker(_) | SpikeError::FrameTooLarge | SpikeError::TooManyMessages => (
            -32020,
            "worker_failure",
            true,
            "worker failed or exceeded a limit".to_owned(),
        ),
        _ => (
            -32603,
            "internal_error",
            false,
            "internal server error".to_owned(),
        ),
    };
    error_response(id, code, cool_code, retryable, &message)
}

fn error_response(id: Value, code: i64, cool_code: &str, retryable: bool, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": {"coolCode": cool_code, "retryable": retryable}
        }
    })
}

async fn write_event_notification(
    writer: &mut (impl AsyncWrite + Unpin),
    event: &Event,
) -> SpikeResult<()> {
    write_json_line(
        writer,
        &json!({"jsonrpc": "2.0", "method": "run/event", "params": event}),
    )
    .await
}

async fn deliver_event(writer: &mut (impl AsyncWrite + Unpin), event: &Event) -> SpikeResult<()> {
    tokio::time::timeout(DELIVERY_TIMEOUT, async {
        write_event_notification(writer, event).await?;
        writer.flush().await?;
        SpikeResult::Ok(())
    })
    .await
    .map_err(|_| SpikeError::Protocol("client delivery timed out".to_owned()))?
}

async fn write_json_with_deadline(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &Value,
) -> SpikeResult<()> {
    tokio::time::timeout(DELIVERY_TIMEOUT, async {
        write_json_line(writer, value).await?;
        writer.flush().await?;
        SpikeResult::Ok(())
    })
    .await
    .map_err(|_| SpikeError::Protocol("client delivery timed out".to_owned()))?
}

async fn write_json_line(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> SpikeResult<()> {
    let mut encoded = serde_json::to_vec(value)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    Ok(())
}

fn normal_mode() -> String {
    "normal".to_owned()
}

const fn before_first_event() -> i64 {
    -1
}

const SPIKE_ACTOR: &str = "local:stdio-owner";
