use std::time::Duration;

use cool_app_server::{AppServer, ServerConfig};
use cool_protocol::{CanonicalEvent, ResponsePayload, RpcId, ServerFrame, StreamFrame};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream, ReadBuf,
    ReadHalf, WriteHalf,
};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

struct Client {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
}

impl Client {
    async fn send(&mut self, id: i64, command: Value) {
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "cool.command",
            "params": {
                "protocolVersion": 1,
                "commandId": format!("command-{id}"),
                "command": command
            }
        });
        self.writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .expect("write request");
        self.writer.flush().await.expect("flush request");
    }

    async fn frame(&mut self) -> ServerFrame {
        let mut line = String::new();
        timeout(Duration::from_secs(2), self.reader.read_line(&mut line))
            .await
            .expect("server response timeout")
            .expect("read server response");
        assert!(!line.is_empty(), "server closed before returning a frame");
        serde_json::from_str(&line).expect("valid generated server frame")
    }

    async fn success(&mut self, expected_id: i64) -> ResponsePayload {
        loop {
            match self.frame().await {
                ServerFrame::Success(success) => {
                    assert_eq!(success.id, RpcId::Integer(expected_id));
                    return success.result;
                }
                ServerFrame::Notification(_) => continue,
                frame => panic!("expected success, got {frame:?}"),
            }
        }
    }

    async fn notification(&mut self) -> cool_protocol::EventEnvelope {
        match self.frame().await {
            ServerFrame::Notification(notification) => match notification.params {
                StreamFrame::Event(event) => *event,
                frame => panic!("expected event notification, got {frame:?}"),
            },
            frame => panic!("expected notification, got {frame:?}"),
        }
    }

    async fn failure(&mut self, expected_id: RpcId) -> cool_protocol::ProtocolError {
        loop {
            match self.frame().await {
                ServerFrame::Failure(failure) => {
                    assert_eq!(failure.id, expected_id);
                    return failure.error;
                }
                ServerFrame::Notification(_) => continue,
                frame => panic!("expected failure, got {frame:?}"),
            }
        }
    }
}

fn connection(server: AppServer) -> (Client, JoinHandle<std::io::Result<()>>) {
    let (client, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    (
        Client {
            reader: BufReader::new(reader),
            writer,
        },
        task,
    )
}

async fn initialize(client: &mut Client, id: i64) {
    client
        .send(
            id,
            json!({
                "method": "initialize",
                "params": {
                    "clientName": "m5-test",
                    "clientVersion": "1",
                    "supportedProtocolVersions": [1],
                    "capabilities": []
                }
            }),
        )
        .await;
    match client.success(id).await {
        ResponsePayload::Initialized(result) => {
            assert_eq!(result.protocol_version, cool_protocol::V1Version::VALUE);
            assert!(result.capabilities.contains("event_catch_up"));
        }
        result => panic!("unexpected initialize result: {result:?}"),
    }
}

async fn create_session(client: &mut Client, id: i64, key: &str) -> String {
    client
        .send(
            id,
            json!({
                "method": "session.create",
                "params": {"idempotencyKey": key, "title": null, "projectKey": null}
            }),
        )
        .await;
    match client.success(id).await {
        ResponsePayload::SessionCreated(result) => result.session_id,
        result => panic!("unexpected create result: {result:?}"),
    }
}

async fn prompt(client: &mut Client, id: i64, session_id: &str, key: &str) -> String {
    client
        .send(
            id,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": key,
                    "sessionId": session_id,
                    "content": [{"type": "text", "text": "hello"}],
                    "model": null
                }
            }),
        )
        .await;
    match client.success(id).await {
        ResponsePayload::PromptAccepted(result) => result.run_id,
        result => panic!("unexpected prompt result: {result:?}"),
    }
}

#[tokio::test]
async fn stdio_shape_creates_ephemeral_session_and_streams_events() {
    let server = AppServer::new(ServerConfig::default());
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "session-key").await;
    let run_id = prompt(&mut client, 3, &session_id, "prompt-key").await;

    let events = [
        client.notification().await,
        client.notification().await,
        client.notification().await,
    ];
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(matches!(events[0].event, CanonicalEvent::RunStarted(_)));
    assert!(matches!(events[1].event, CanonicalEvent::ContentDelta(_)));
    assert!(matches!(events[2].event, CanonicalEvent::RunCompleted(_)));
    assert_eq!(server.events_for_run(&run_id).await.unwrap(), events);

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn prompt_idempotency_prevents_duplicate_execution() {
    let server = AppServer::new(ServerConfig::default());
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "session-key").await;
    let first = prompt(&mut client, 3, &session_id, "same-prompt").await;
    let second = prompt(&mut client, 4, &session_id, "same-prompt").await;
    assert_eq!(first, second);
    assert_eq!(server.prompt_executions().await, 1);

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn cancellation_is_terminal_and_replayable() {
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "session-key").await;
    let run_id = prompt(&mut client, 3, &session_id, "prompt-key").await;
    assert!(matches!(
        client.notification().await.event,
        CanonicalEvent::RunStarted(_)
    ));

    client
        .send(
            4,
            json!({
                "method": "run.cancel",
                "params": {"idempotencyKey": "cancel-key", "runId": run_id, "reason": "test"}
            }),
        )
        .await;
    assert!(matches!(
        client.success(4).await,
        ResponsePayload::RunCancelled(_)
    ));
    match client.notification().await.event {
        CanonicalEvent::RunCancelled(terminal) => assert_eq!(terminal.reason, "test"),
        event => panic!("expected run.cancelled, got {event:?}"),
    }
    client
        .send(
            5,
            json!({
                "method": "run.cancel",
                "params": {"idempotencyKey": "cancel-key", "runId": run_id, "reason": "test"}
            }),
        )
        .await;
    match client.success(5).await {
        ResponsePayload::RunCancelled(result) => assert!(result.accepted),
        result => panic!("unexpected repeated cancel result: {result:?}"),
    }

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn disconnect_does_not_overwrite_an_accepted_cancel_reason() {
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "reason-session").await;
    let run_id = prompt(&mut client, 3, &session_id, "reason-prompt").await;
    client
        .send(
            4,
            json!({
                "method": "run.cancel",
                "params": {"idempotencyKey": "reason-cancel", "runId": run_id, "reason": "keep-me"}
            }),
        )
        .await;
    assert!(matches!(
        client.success(4).await,
        ResponsePayload::RunCancelled(_)
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
    let events = server.events_for_run(&run_id).await.unwrap();
    match &events.last().unwrap().event {
        CanonicalEvent::RunCancelled(terminal) => assert_eq!(terminal.reason, "keep-me"),
        event => panic!("expected run.cancelled, got {event:?}"),
    }
}

#[tokio::test]
async fn changed_input_for_an_idempotency_key_is_rejected() {
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut client, task) = connection(server);
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "session-key").await;
    client
        .send(
            3,
            json!({
                "method": "session.create",
                "params": {"idempotencyKey": "session-key", "title": "changed", "projectKey": null}
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(3)).await.cool_code,
        "idempotency_conflict"
    );

    let run_id = prompt(&mut client, 4, &session_id, "prompt-key").await;
    client
        .send(
            5,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": "prompt-key",
                    "sessionId": session_id,
                    "content": [{"type": "text", "text": "changed"}],
                    "model": null
                }
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(5)).await.cool_code,
        "idempotency_conflict"
    );

    client
        .send(
            6,
            json!({
                "method": "run.cancel",
                "params": {"idempotencyKey": "cancel-key", "runId": run_id, "reason": "first"}
            }),
        )
        .await;
    assert!(matches!(
        client.success(6).await,
        ResponsePayload::RunCancelled(_)
    ));
    client
        .send(
            7,
            json!({
                "method": "run.cancel",
                "params": {"idempotencyKey": "cancel-key", "runId": "another-run", "reason": "first"}
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(7)).await.cool_code,
        "idempotency_conflict"
    );

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn idempotency_conflict_precedes_content_and_output_validation() {
    let config = ServerConfig {
        max_frame_bytes: 768,
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "validation-order-session").await;
    let run_id = prompt(&mut client, 3, &session_id, "validation-order-prompt").await;

    client
        .send(
            4,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": "validation-order-prompt",
                    "sessionId": session_id,
                    "content": [{"type": "artifact", "artifactId": "artifact-1"}],
                    "model": null
                }
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(4)).await.cool_code,
        "idempotency_conflict"
    );

    client
        .send(
            5,
            json!({
                "method": "run.cancel",
                "params": {
                    "idempotencyKey": "validation-order-cancel",
                    "runId": run_id,
                    "reason": "first"
                }
            }),
        )
        .await;
    assert!(matches!(
        client.success(5).await,
        ResponsePayload::RunCancelled(_)
    ));
    client
        .send(
            6,
            json!({
                "method": "run.cancel",
                "params": {
                    "idempotencyKey": "validation-order-cancel",
                    "runId": run_id,
                    "reason": "x".repeat(400)
                }
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(6)).await.cool_code,
        "idempotency_conflict"
    );
    assert_eq!(server.prompt_executions().await, 1);

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn overload_is_a_structured_retryable_error() {
    let config = ServerConfig {
        max_in_flight: 1,
        request_delay: Duration::from_millis(100),
        ..ServerConfig::default()
    };
    let (mut client, task) = connection(AppServer::new(config));
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "overload-session").await;
    let load = json!({"method": "session.load", "params": {"sessionId": session_id}});
    client.send(3, load.clone()).await;
    client.send(4, load).await;
    match client.frame().await {
        ServerFrame::Failure(failure) => {
            assert_eq!(failure.id, RpcId::Integer(4));
            assert_eq!(failure.error.cool_code, "server_overloaded");
            assert!(failure.error.retryable);
        }
        frame => panic!("expected overload failure, got {frame:?}"),
    }
    assert!(matches!(
        client.success(3).await,
        ResponsePayload::SessionLoaded(_)
    ));

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn reconnect_catches_up_without_gap_or_duplicate_side_effect() {
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut first, first_task) = connection(server.clone());
    initialize(&mut first, 1).await;
    let session_id = create_session(&mut first, 2, "session-key").await;
    let run_id = prompt(&mut first, 3, &session_id, "prompt-key").await;
    assert_eq!(first.notification().await.seq, 1);
    drop(first);
    first_task
        .await
        .expect("server task")
        .expect("clean disconnect");

    timeout(Duration::from_secs(1), async {
        loop {
            if server.events_for_run(&run_id).await.unwrap().len() == 2 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("disconnect cancellation becomes terminal");

    let (mut second, second_task) = connection(server.clone());
    initialize(&mut second, 10).await;
    let retried_run_id = prompt(&mut second, 11, &session_id, "prompt-key").await;
    assert_eq!(retried_run_id, run_id);
    assert_eq!(server.prompt_executions().await, 1);
    second
        .send(
            12,
            json!({
                "method": "run.events",
                "params": {"runId": run_id, "afterSeq": 1, "limit": 10}
            }),
        )
        .await;
    match second.success(12).await {
        ResponsePayload::EventPage(page) => {
            assert_eq!(page.events.len(), 1);
            assert_eq!(page.events[0].seq, 2);
            assert!(matches!(
                page.events[0].event,
                CanonicalEvent::RunCancelled(_)
            ));
            assert!(!page.has_more);
        }
        result => panic!("unexpected event page: {result:?}"),
    }

    drop(second);
    second_task
        .await
        .expect("server task")
        .expect("clean disconnect");
}

#[tokio::test]
async fn event_replay_paginates_by_serialized_frame_size_without_gaps() {
    let config = ServerConfig {
        max_frame_bytes: 900,
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut client, task) = connection(server);
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "byte-page-session").await;
    let run_id = prompt(&mut client, 3, &session_id, "byte-page-prompt").await;
    for _ in 0..3 {
        let _ = client.notification().await;
    }

    let mut after_seq = None;
    let mut sequences = Vec::new();
    let mut request_id = 10;
    loop {
        client
            .send(
                request_id,
                json!({
                    "method": "run.events",
                    "params": {"runId": run_id, "afterSeq": after_seq, "limit": 10}
                }),
            )
            .await;
        let page = match client.success(request_id).await {
            ResponsePayload::EventPage(page) => page,
            result => panic!("unexpected event page: {result:?}"),
        };
        assert!(!page.events.is_empty());
        sequences.extend(page.events.iter().map(|event| event.seq));
        after_seq = page.next_cursor.and_then(|cursor| cursor.after_seq);
        request_id += 1;
        if !page.has_more {
            break;
        }
    }
    assert_eq!(sequences, [1, 2, 3]);
    assert!(request_id > 11, "the byte limit must force multiple pages");

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn disconnect_during_prompt_dispatch_does_not_orphan_or_duplicate_the_run() {
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        request_delay: Duration::from_millis(30),
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut first, first_task) = connection(server.clone());
    initialize(&mut first, 1).await;
    let session_id = create_session(&mut first, 2, "dispatch-session").await;
    first
        .send(
            3,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": "dispatch-prompt",
                    "sessionId": session_id,
                    "content": [{"type": "text", "text": "hello"}],
                    "model": null
                }
            }),
        )
        .await;
    drop(first);
    first_task
        .await
        .expect("server task")
        .expect("disconnect after dispatch");

    let (mut second, second_task) = connection(server.clone());
    initialize(&mut second, 10).await;
    let run_id = prompt(&mut second, 11, &session_id, "dispatch-prompt").await;
    assert_eq!(server.prompt_executions().await, 1);
    let events = server.events_for_run(&run_id).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events.last().unwrap().event,
        CanonicalEvent::RunCancelled(_)
    ));

    drop(second);
    second_task
        .await
        .expect("server task")
        .expect("clean disconnect");
}

#[tokio::test]
async fn oversized_frame_does_not_desynchronize_the_next_request() {
    let config = ServerConfig {
        max_frame_bytes: 512,
        ..ServerConfig::default()
    };
    let (mut client, task) = connection(AppServer::new(config));
    client
        .writer
        .write_all(format!("{{\"padding\":\"{}\"}}\n", "x".repeat(1024)).as_bytes())
        .await
        .unwrap();
    match client.frame().await {
        ServerFrame::Failure(failure) => assert_eq!(failure.error.cool_code, "frame_too_large"),
        frame => panic!("expected frame limit error, got {frame:?}"),
    }

    initialize(&mut client, 1).await;
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn pipelined_initialize_establishes_the_boundary_before_dispatch() {
    let (mut client, task) = connection(AppServer::new(ServerConfig::default()));
    let initialize = json!({
        "method": "initialize",
        "params": {
            "clientName": "pipeline-test",
            "clientVersion": "1",
            "supportedProtocolVersions": [1],
            "capabilities": []
        }
    });
    client.send(1, initialize).await;
    client
        .send(
            2,
            json!({
                "method": "session.create",
                "params": {"idempotencyKey": "pipeline", "title": null, "projectKey": null}
            }),
        )
        .await;
    assert!(matches!(
        client.success(1).await,
        ResponsePayload::Initialized(_)
    ));
    assert!(matches!(
        client.success(2).await,
        ResponsePayload::SessionCreated(_)
    ));
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn session_has_one_active_run_and_clears_it_at_terminal_state() {
    let config = ServerConfig {
        event_delay: Duration::from_secs(10),
        ..ServerConfig::default()
    };
    let (mut client, task) = connection(AppServer::new(config));
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "lifecycle-session").await;
    let run_id = prompt(&mut client, 3, &session_id, "first-prompt").await;
    client
        .send(
            4,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": "second-prompt",
                    "sessionId": session_id,
                    "content": [{"type": "text", "text": "second"}],
                    "model": null
                }
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(4)).await.cool_code,
        "session_run_active"
    );
    client
        .send(
            5,
            json!({
                "method": "run.cancel",
                "params": {"idempotencyKey": "lifecycle-cancel", "runId": run_id, "reason": null}
            }),
        )
        .await;
    assert!(matches!(
        client.success(5).await,
        ResponsePayload::RunCancelled(_)
    ));
    loop {
        if matches!(
            client.notification().await.event,
            CanonicalEvent::RunCancelled(_)
        ) {
            break;
        }
    }
    client
        .send(
            6,
            json!({"method": "session.load", "params": {"sessionId": session_id}}),
        )
        .await;
    match client.success(6).await {
        ResponsePayload::SessionLoaded(result) => assert_eq!(result.active_run_id, None),
        result => panic!("unexpected load result: {result:?}"),
    }
    assert_ne!(
        prompt(&mut client, 7, &session_id, "second-prompt").await,
        run_id
    );
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn json_rpc_errors_distinguish_parse_request_and_params() {
    let (mut client, task) = connection(AppServer::new(ServerConfig::default()));
    for (raw, id, rpc_code, cool_code) in [
        ("{\n".to_owned(), RpcId::Null, -32700, "parse_error"),
        (
            format!(
                "{}\n",
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "cool.command",
                    "params": {"protocolVersion": 1, "commandId": "x", "command": {"method": "initialize", "params": {"clientName": "x", "clientVersion": "1", "supportedProtocolVersions": [1], "capabilities": []}}},
                    "extra": true
                })
            ),
            RpcId::Integer(1),
            -32600,
            "invalid_request",
        ),
        (
            format!(
                "{}\n",
                json!({
                    "jsonrpc": "2.0", "id": 3, "method": "unknown.method",
                    "params": {}
                })
            ),
            RpcId::Integer(3),
            -32601,
            "method_not_found",
        ),
        (
            format!(
                "{}\n",
                json!({
                    "jsonrpc": "2.0", "id": 1.5, "method": "cool.command",
                    "params": {"protocolVersion": 1, "commandId": "x", "command": {"method": "initialize", "params": {"clientName": "x", "clientVersion": "1", "supportedProtocolVersions": [1], "capabilities": []}}}
                })
            ),
            RpcId::Null,
            -32600,
            "invalid_request",
        ),
        (
            format!(
                "{}\n",
                json!({
                    "jsonrpc": "2.0", "id": 2, "method": "cool.command",
                    "params": {"protocolVersion": 1, "commandId": "x", "command": {"method": "session.load", "params": {}}}
                })
            ),
            RpcId::Integer(2),
            -32602,
            "invalid_params",
        ),
    ] {
        client.writer.write_all(raw.as_bytes()).await.unwrap();
        client.writer.flush().await.unwrap();
        let error = client.failure(id).await;
        assert_eq!(error.rpc_code, rpc_code);
        assert_eq!(error.cool_code, cool_code);
    }
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn oversized_outbound_frame_becomes_a_bounded_structured_error() {
    let config = ServerConfig {
        max_frame_bytes: 768,
        ..ServerConfig::default()
    };
    let server = AppServer::new(config);
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "output-session").await;
    client
        .send(
            3,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": "large-output",
                    "sessionId": session_id,
                    "content": [{"type": "text", "text": "x".repeat(400)}],
                    "model": null
                }
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(3)).await.cool_code,
        "outbound_frame_too_large"
    );
    assert_eq!(server.prompt_executions().await, 0);
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn unsupported_multimodal_parts_fail_closed_before_run_creation() {
    let server = AppServer::new(ServerConfig::default());
    let (mut client, task) = connection(server.clone());
    initialize(&mut client, 1).await;
    let session_id = create_session(&mut client, 2, "unsupported-session").await;
    client
        .send(
            3,
            json!({
                "method": "session.prompt",
                "params": {
                    "idempotencyKey": "unsupported-prompt",
                    "sessionId": session_id,
                    "content": [{"type": "artifact", "artifactId": "artifact-1"}],
                    "model": null
                }
            }),
        )
        .await;
    assert_eq!(
        client.failure(RpcId::Integer(3)).await.cool_code,
        "unsupported_content_part"
    );
    assert_eq!(server.prompt_executions().await, 0);
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

struct StalledWriterIo {
    input: Vec<u8>,
    offset: usize,
}

impl AsyncRead for StalledWriterIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.offset < self.input.len() {
            let remaining = &self.input[self.offset..];
            let copied = remaining.len().min(buffer.remaining());
            buffer.put_slice(&remaining[..copied]);
            self.offset += copied;
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Pending
        }
    }
}

impl AsyncWrite for StalledWriterIo {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Pending
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn non_reading_client_hits_delivery_deadline_and_server_exits() {
    let config = ServerConfig {
        delivery_timeout: Duration::from_millis(20),
        write_timeout: Duration::from_millis(20),
        ..ServerConfig::default()
    };
    let input = format!(
        "{}\n",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "cool.command",
            "params": {"protocolVersion": 1, "commandId": "x", "command": {"method": "initialize", "params": {"clientName": "x", "clientVersion": "1", "supportedProtocolVersions": [1], "capabilities": []}}}
        })
    )
    .into_bytes();
    let result = timeout(
        Duration::from_secs(1),
        AppServer::new(config).serve_io(StalledWriterIo { input, offset: 0 }),
    )
    .await
    .expect("delivery deadline must terminate the connection")
    .expect_err("stalled writer returns a timeout error");
    assert_eq!(result.kind(), std::io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn queue_enqueue_timeout_detaches_the_whole_connection() {
    let config = ServerConfig {
        outbound_queue: 1,
        delivery_timeout: Duration::from_millis(20),
        write_timeout: Duration::from_secs(5),
        ..ServerConfig::default()
    };
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "cool.command",
        "params": {"protocolVersion": 1, "commandId": "init", "command": {"method": "initialize", "params": {"clientName": "x", "clientVersion": "1", "supportedProtocolVersions": [1], "capabilities": []}}}
    });
    let mut lines = vec![initialize.to_string()];
    for id in 2..8 {
        lines.push(
            json!({
                "jsonrpc": "2.0", "id": id, "method": "cool.command",
                "params": {"protocolVersion": 1, "commandId": format!("load-{id}"), "command": {"method": "session.load", "params": {"sessionId": "missing"}}}
            })
            .to_string(),
        );
    }
    let input = format!("{}\n", lines.join("\n")).into_bytes();
    let result = timeout(
        Duration::from_secs(1),
        AppServer::new(config).serve_io(StalledWriterIo { input, offset: 0 }),
    )
    .await
    .expect("enqueue deadline must detach before the longer writer deadline")
    .expect_err("queue timeout terminates the connection");
    assert_eq!(result.kind(), std::io::ErrorKind::TimedOut);
}
