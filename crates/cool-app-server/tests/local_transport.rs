use std::time::Duration;

use cool_app_server::{AppServer, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

#[cfg(unix)]
#[tokio::test]
async fn local_socket_accepts_protocol_frames() {
    let endpoint = std::env::temp_dir().join(format!("cool-m5-{}.sock", Uuid::new_v4()));
    let server = AppServer::new(ServerConfig::default());
    let endpoint_for_server = endpoint.clone();
    let task = tokio::spawn(async move { server.serve_local(&endpoint_for_server).await });
    let stream = timeout(Duration::from_secs(2), async {
        loop {
            match tokio::net::UnixStream::connect(&endpoint).await {
                Ok(stream) => break stream,
                Err(_) => sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("local socket becomes ready");
    assert_parse_error(stream).await;
    task.abort();
    let _ = std::fs::remove_file(endpoint);
}

#[cfg(windows)]
#[tokio::test]
async fn local_socket_accepts_protocol_frames() {
    use tokio::net::windows::named_pipe::ClientOptions;

    let endpoint = format!(r"\\.\pipe\cool-m5-{}", Uuid::new_v4());
    let server = AppServer::new(ServerConfig::default());
    let endpoint_for_server = std::path::PathBuf::from(&endpoint);
    let task = tokio::spawn(async move { server.serve_local(&endpoint_for_server).await });
    let stream = timeout(Duration::from_secs(2), async {
        loop {
            match ClientOptions::new().open(&endpoint) {
                Ok(stream) => break stream,
                Err(_) => sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .expect("named pipe becomes ready");
    assert_parse_error(stream).await;
    task.abort();
}

async fn assert_parse_error<T>(stream: T)
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    writer.write_all(b"not-json\n").await.unwrap();
    writer.flush().await.unwrap();
    let mut reader = BufReader::new(reader);
    let mut response = String::new();
    timeout(Duration::from_secs(2), reader.read_line(&mut response))
        .await
        .expect("protocol response timeout")
        .expect("read protocol response");
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["error"]["coolCode"], "parse_error");
}
