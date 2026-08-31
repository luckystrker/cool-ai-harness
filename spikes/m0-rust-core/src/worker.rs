use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::framing::{MAX_FRAME_BYTES, read_limited_line};
use crate::model::{SpikeError, SpikeResult};

const MAX_WORKER_MESSAGES: usize = 64;
const MAX_WORKER_BYTES: usize = 256 * 1024;
const WORKER_CHANNEL_CAPACITY: usize = 8;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub run_id: String,
    pub prompt: String,
    pub mode: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum WorkerMessage {
    ContentDelta {
        text: String,
    },
    ToolIntent {
        call_id: String,
        name: String,
        arguments: Value,
    },
}

#[derive(Debug)]
pub struct WorkerExit {
    pub success: bool,
    pub exit_code: Option<i32>,
}

pub struct WorkerStream {
    pub messages: mpsc::Receiver<SpikeResult<WorkerMessage>>,
    pub completion: tokio::task::JoinHandle<SpikeResult<WorkerExit>>,
}

pub async fn spawn_worker(executable: &Path, request: &WorkerRequest) -> SpikeResult<WorkerStream> {
    let mut child = Command::new(executable)
        .arg("worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| SpikeError::Worker("worker stdin unavailable".to_owned()))?;
    let encoded = serde_json::to_vec(request)?;
    stdin.write_all(&encoded).await?;
    stdin.write_all(b"\n").await?;
    stdin.shutdown().await?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SpikeError::Worker("worker stdout unavailable".to_owned()))?;
    let (sender, receiver) = mpsc::channel(WORKER_CHANNEL_CAPACITY);
    let completion = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async move {
            let mut reader = BufReader::new(stdout);
            let mut message_count = 0_usize;
            let mut total_bytes = 0_usize;
            loop {
                let frame = match read_limited_line(&mut reader).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                        let _ = child.kill().await;
                        return Err(SpikeError::Worker("invalid worker frame".to_owned()));
                    }
                };
                message_count += 1;
                total_bytes = total_bytes.saturating_add(frame.len());
                if message_count > MAX_WORKER_MESSAGES || total_bytes > MAX_WORKER_BYTES {
                    let _ = sender.send(Err(SpikeError::TooManyMessages)).await;
                    let _ = child.kill().await;
                    return Err(SpikeError::Worker(
                        "worker output limit exceeded".to_owned(),
                    ));
                }
                let message = serde_json::from_slice(&frame)?;
                if sender.send(Ok(message)).await.is_err() {
                    let _ = child.kill().await;
                    return Err(SpikeError::Worker("worker consumer closed".to_owned()));
                }
            }
            let status = child.wait().await?;
            Ok(WorkerExit {
                success: status.success(),
                exit_code: status.code(),
            })
        })
        .await
        .map_err(|_| SpikeError::Worker("worker deadline exceeded".to_owned()))?
    });
    Ok(WorkerStream {
        messages: receiver,
        completion,
    })
}

pub async fn worker_stdio() -> SpikeResult<()> {
    let mut reader = BufReader::new(tokio::io::stdin());
    let frame = read_limited_line(&mut reader)
        .await?
        .ok_or_else(|| SpikeError::Protocol("worker request missing".to_owned()))?;
    let request: WorkerRequest = serde_json::from_slice(&frame)?;
    let mut stdout = tokio::io::stdout();

    if request.mode == "oversized" {
        stdout.write_all(&vec![b'x'; MAX_FRAME_BYTES + 1]).await?;
        stdout.flush().await?;
        return Ok(());
    }
    if request.mode == "message-flood" {
        for index in 0..=MAX_WORKER_MESSAGES {
            write_message(
                &mut stdout,
                &WorkerMessage::ContentDelta {
                    text: format!("{index}"),
                },
            )
            .await?;
        }
        stdout.flush().await?;
        return Ok(());
    }

    write_message(
        &mut stdout,
        &WorkerMessage::ContentDelta {
            text: format!("scripted:{}", request.prompt),
        },
    )
    .await?;
    stdout.flush().await?;

    if request.mode == "crash" {
        std::process::exit(42);
    }
    if request.mode == "delayed" {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let (call_id, value) = match request.mode.as_str() {
        "collision-a" => ("shared-marker".to_owned(), json!("first")),
        "collision-b" => ("shared-marker".to_owned(), json!("second")),
        "malformed" => (format!("{}:marker", request.run_id), json!(42)),
        _ => (
            format!("{}:marker", request.run_id),
            json!("approved-effect"),
        ),
    };
    write_message(
        &mut stdout,
        &WorkerMessage::ToolIntent {
            call_id,
            name: "write_marker".to_owned(),
            arguments: json!({"value": value}),
        },
    )
    .await?;
    stdout.flush().await?;
    Ok(())
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &WorkerMessage,
) -> SpikeResult<()> {
    let mut encoded = serde_json::to_vec(message)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    Ok(())
}
