//! M11 HTTP facade integration tests.
//!
//! Each test starts the real router on an ephemeral loopback port, drives it
//! with a cookie-aware HTTP client, and asserts the canonical App Protocol
//! response shapes (never an HTTP-specific business model).

use std::net::SocketAddr;
use std::time::Duration;

use cool_app_server::{AppServer, ServerConfig};
use cool_http::{HttpFacade, ServeOptions, ServeProfile};
use cool_protocol::{
    Command, CommandEnvelope, CoolCommandMethod, IdempotencyKey, JsonRpcV2, RpcId, RpcRequest,
    ServerFrame, SessionListParams, V1Version,
};
use futures_util::StreamExt;

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("valid idempotency key")
}
use cool_state::DurableStore;

struct Serving {
    base: String,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(server: AppServer, options: ServeOptions) -> Serving {
    let facade = HttpFacade::new(server, options).expect("facade config");
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ephemeral port");
    let address = listener.local_addr().expect("local addr");
    let task = tokio::spawn(async move { facade.serve(listener).await });
    Serving {
        base: format!("http://{address}"),
        task,
    }
}

fn durable_server() -> AppServer {
    durable_server_with(256, Duration::ZERO)
}

fn durable_server_with(page_limit: u16, event_delay: Duration) -> AppServer {
    AppServer::with_store(
        ServerConfig {
            event_page_limit: page_limit,
            event_delay,
            ..ServerConfig::default()
        },
        DurableStore::in_memory().expect("in-memory durable store"),
    )
    .expect("server")
}

async fn create_and_prompt(client: &reqwest::Client, base: &str) -> String {
    let session = created(
        post_rpc(
            client,
            base,
            &request(
                1,
                Command::SessionCreate(cool_protocol::SessionCreateParams {
                    idempotency_key: key("session-1"),
                    title: None,
                    project_key: None,
                }),
            ),
        )
        .await,
    );
    accepted(
        post_rpc(
            client,
            base,
            &request(
                2,
                Command::SessionPrompt(cool_protocol::SessionPromptParams {
                    idempotency_key: key("prompt-1"),
                    session_id: session,
                    content: vec![cool_protocol::ContentPart::Text {
                        text: "hello".to_owned(),
                    }],
                    model: None,
                    plan_mode: false,
                    system_prompt: None,
                }),
            ),
        )
        .await,
    )
}

fn max_event_id(body: &str) -> u64 {
    body.lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .filter_map(|value| value.trim().parse().ok())
        .max()
        .unwrap_or(0)
}

fn request(id: i64, command: Command) -> RpcRequest {
    RpcRequest {
        jsonrpc: JsonRpcV2::VALUE,
        id: RpcId::Integer(id),
        method: CoolCommandMethod::VALUE,
        params: CommandEnvelope {
            protocol_version: V1Version::VALUE,
            command_id: format!("test-{id}"),
            command,
        },
    }
}

async fn post_rpc(client: &reqwest::Client, base: &str, body: &RpcRequest) -> ServerFrame {
    let response = client
        .post(format!("{base}/api/rpc"))
        .json(body)
        .send()
        .await
        .expect("rpc request");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json::<ServerFrame>().await.expect("server frame")
}

fn created(frame: ServerFrame) -> String {
    match frame {
        ServerFrame::Success(success) => match success.result {
            cool_protocol::ResponsePayload::SessionCreated(result) => result.session_id,
            other => panic!("expected session.created, got {other:?}"),
        },
        ServerFrame::Failure(failure) => panic!("unexpected failure: {:?}", failure.error),
        ServerFrame::Notification(notification) => {
            panic!("unexpected notification: {notification:?}")
        }
    }
}

fn listed(frame: ServerFrame) -> Vec<cool_protocol::SessionSummary> {
    match frame {
        ServerFrame::Success(success) => match success.result {
            cool_protocol::ResponsePayload::SessionListed(result) => result.sessions,
            other => panic!("expected session.list, got {other:?}"),
        },
        ServerFrame::Failure(failure) => panic!("unexpected failure: {:?}", failure.error),
        ServerFrame::Notification(notification) => {
            panic!("unexpected notification: {notification:?}")
        }
    }
}

fn accepted(frame: ServerFrame) -> String {
    match frame {
        ServerFrame::Success(success) => match success.result {
            cool_protocol::ResponsePayload::PromptAccepted(result) => result.run_id,
            other => panic!("expected prompt.accepted, got {other:?}"),
        },
        ServerFrame::Failure(failure) => panic!("unexpected failure: {:?}", failure.error),
        ServerFrame::Notification(notification) => {
            panic!("unexpected notification: {notification:?}")
        }
    }
}

#[tokio::test]
async fn health_is_public_and_reports_the_web_facade() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let response = reqwest::get(format!("{}/api/health", serving.base))
        .await
        .expect("health");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("health json");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["runtime"], "rust-trusted-core");
    assert_eq!(body["phase"], "M11");
}

#[tokio::test]
async fn rpc_round_trips_canonical_commands() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");

    let session = created(
        post_rpc(
            &client,
            &serving.base,
            &request(
                1,
                Command::SessionCreate(cool_protocol::SessionCreateParams {
                    idempotency_key: key("session-1"),
                    title: Some("demo".to_owned()),
                    project_key: Some("project".to_owned()),
                }),
            ),
        )
        .await,
    );

    let sessions = listed(
        post_rpc(
            &client,
            &serving.base,
            &request(
                2,
                Command::SessionList(SessionListParams {
                    project_key: Some("project".to_owned()),
                    limit: 50,
                }),
            ),
        )
        .await,
    );
    assert!(sessions.iter().any(|summary| summary.session_id == session));
}

#[tokio::test]
async fn events_stream_replays_durable_events_and_ends_on_terminal() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");

    let session = created(
        post_rpc(
            &client,
            &serving.base,
            &request(
                1,
                Command::SessionCreate(cool_protocol::SessionCreateParams {
                    idempotency_key: key("session-1"),
                    title: None,
                    project_key: None,
                }),
            ),
        )
        .await,
    );
    let run = accepted(
        post_rpc(
            &client,
            &serving.base,
            &request(
                2,
                Command::SessionPrompt(cool_protocol::SessionPromptParams {
                    idempotency_key: key("prompt-1"),
                    session_id: session,
                    content: vec![cool_protocol::ContentPart::Text {
                        text: "hello".to_owned(),
                    }],
                    model: None,
                    plan_mode: false,
                    system_prompt: None,
                }),
            ),
        )
        .await,
    );

    let body = read_sse_for(
        &client,
        &format!(
            "{}/api/events?runId={run}&afterSeq=0&limit=64",
            serving.base
        ),
        Duration::from_secs(10),
    )
    .await;

    let kinds: Vec<&str> = body
        .lines()
        .filter_map(|line| line.strip_prefix("event: "))
        .collect();
    assert!(kinds.contains(&"run.event"), "events:\n{body}");
    assert!(kinds.contains(&"end"), "events:\n{body}");
    assert!(body.contains("run.started"), "events:\n{body}");
    assert!(body.contains("run.completed"), "events:\n{body}");

    // Canonical reconnect: a cursor past the terminal seq replays no durable
    // run events, so the client cannot double-apply anything. The stream then
    // stays live for future events (the run is already terminal, so none
    // arrive) instead of fabricating a synthetic terminal frame.
    let replay = read_sse_for(
        &client,
        &format!(
            "{}/api/events?runId={run}&afterSeq=1000000&limit=64",
            serving.base
        ),
        Duration::from_secs(2),
    )
    .await;
    assert!(!replay.contains("event: run.event"), "replay:\n{replay}");
}

#[tokio::test]
async fn opaque_origin_is_rejected_in_local_profile() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/rpc", serving.base))
        .header("origin", "null")
        .json(&request(
            1,
            Command::SessionList(SessionListParams {
                project_key: None,
                limit: 10,
            }),
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["coolCode"], "origin_not_allowed");
}

#[tokio::test]
async fn cross_site_event_stream_is_rejected() {
    // A cross-site page must not be able to mint protocol connections through
    // the GET SSE endpoint either.
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/events?runId=whatever", serving.base))
        .header("origin", "https://evil.example.com")
        .send()
        .await
        .expect("events");
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    let missing = client
        .get(format!("{}/api/events", serving.base))
        .send()
        .await
        .expect("events");
    assert_eq!(missing.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = missing.json().await.expect("error json");
    assert_eq!(body["coolCode"], "missing_run_id");
}

#[tokio::test]
async fn anonymous_event_stream_is_rejected() {
    // A cross-site no-cors GET carries no Origin and no SameSite cookie; it
    // must not be able to spawn a durable protocol connection.
    let serving = start(durable_server(), ServeOptions::default()).await;
    let response = reqwest::get(format!("{}/api/events?runId=whatever", serving.base))
        .await
        .expect("events");
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["coolCode"], "client_identity_required");
}

#[tokio::test]
async fn token_authenticated_event_stream_may_mint_identity() {
    // The anonymous-SSE guard must not fire when the request already passed
    // token authentication; the token (not the cookie) is the identity here.
    let serving = start(
        durable_server(),
        ServeOptions {
            token: Some("0123456789abcdef".to_owned()),
            ..ServeOptions::default()
        },
    )
    .await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer 0123456789abcdef".parse().expect("header"),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("client");
    let body = read_sse_for(
        &client,
        &format!("{}/api/events?runId=missing&limit=8", serving.base),
        Duration::from_secs(3),
    )
    .await;
    assert!(body.contains("event: error"), "token stream:\n{body}");
}

#[tokio::test]
async fn last_event_id_header_takes_precedence_over_after_seq() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");
    let run = create_and_prompt(&client, &serving.base).await;

    let first = read_sse_for(
        &client,
        &format!(
            "{}/api/events?runId={run}&afterSeq=0&limit=64",
            serving.base
        ),
        Duration::from_secs(5),
    )
    .await;
    let last = max_event_id(&first);
    assert!(last >= 1, "expected durable event ids:\n{first}");

    // Query says afterSeq=0, but Last-Event-ID says we already saw `last`; the
    // header must win so a reconnect cannot replay old events.
    let response = client
        .get(format!(
            "{}/api/events?runId={run}&afterSeq=0&limit=64",
            serving.base
        ))
        .header("last-event-id", last.to_string())
        .send()
        .await
        .expect("events");
    let mut stream = response.bytes_stream();
    let mut body = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Some(Ok(bytes))) = tokio::time::timeout(
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        stream.next(),
    )
    .await
    {
        body.push_str(&String::from_utf8_lossy(&bytes));
    }
    assert!(!body.contains("event: run.event"), "replay:\n{body}");
}

#[tokio::test]
async fn live_phase_streams_events_emitted_after_catch_up() {
    // A slow scripted provider makes the run outlive the first SSE poll, so
    // the terminal event can only arrive through the live broadcast path.
    let serving = start(
        durable_server_with(256, Duration::from_millis(250)),
        ServeOptions::default(),
    )
    .await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");
    let run = create_and_prompt(&client, &serving.base).await;
    let body = read_sse_for(
        &client,
        &format!(
            "{}/api/events?runId={run}&afterSeq=0&limit=64",
            serving.base
        ),
        Duration::from_secs(8),
    )
    .await;
    assert!(body.contains("run.completed"), "live events:\n{body}");
    assert!(body.contains("event: end"), "live events:\n{body}");
}

#[tokio::test]
async fn invalid_client_header_does_not_rotate_the_cookie_identity() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");

    let first = client
        .get(format!("{}/api/health", serving.base))
        .send()
        .await
        .expect("health");
    assert!(
        first.headers().get("set-cookie").is_some(),
        "first response must mint a cookie"
    );

    // A too-short header is invalid; the valid cookie must still be reused
    // instead of rotating the identity.
    let second = client
        .get(format!("{}/api/health", serving.base))
        .header("x-cool-client", "x")
        .send()
        .await
        .expect("health");
    assert!(
        second.headers().get("set-cookie").is_none(),
        "invalid header rotated the identity"
    );
}

#[tokio::test]
async fn sse_limit_is_clamped_to_the_server_page_limit() {
    let serving = start(
        durable_server_with(8, Duration::ZERO),
        ServeOptions::default(),
    )
    .await;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client");
    let run = create_and_prompt(&client, &serving.base).await;
    let body = read_sse_for(
        &client,
        &format!(
            "{}/api/events?runId={run}&afterSeq=0&limit=256",
            serving.base
        ),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        !body.contains("event: error"),
        "clamped limit failed:\n{body}"
    );
    assert!(body.contains("run.completed"), "events:\n{body}");
}

/// Reads an SSE response with a hard deadline so a non-terminating stream
/// fails the assertion instead of hanging the suite.
async fn read_sse_for(client: &reqwest::Client, url: &str, window: Duration) -> String {
    let response = client.get(url).send().await.expect("events");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut stream = response.bytes_stream();
    let deadline = tokio::time::Instant::now() + window;
    let mut body = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            body.push_str("\n<sse-deadline>");
            break;
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(bytes))) => body.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Some(Err(error))) => {
                body.push_str(&format!("\n<sse-error {error}>"));
                break;
            }
            Ok(None) => break,
            Err(_) => {
                body.push_str("\n<sse-deadline>");
                break;
            }
        }
    }
    body
}

#[tokio::test]
async fn local_profile_rejects_cross_origin_mutations() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/rpc", serving.base))
        .header("origin", "https://evil.example.com")
        .json(&request(
            1,
            Command::SessionList(SessionListParams {
                project_key: None,
                limit: 10,
            }),
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert_eq!(body["coolCode"], "origin_not_allowed");
}

#[tokio::test]
async fn configured_token_gates_the_api_but_not_health() {
    let options = ServeOptions {
        token: Some("0123456789abcdef".to_owned()),
        ..ServeOptions::default()
    };
    let serving = start(durable_server(), options).await;
    let client = reqwest::Client::new();

    let health = client
        .get(format!("{}/api/health", serving.base))
        .send()
        .await
        .expect("health");
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    let unauthorized = client
        .post(format!("{}/api/rpc", serving.base))
        .json(&request(
            1,
            Command::SessionList(SessionListParams {
                project_key: None,
                limit: 10,
            }),
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let authorized = client
        .post(format!("{}/api/rpc", serving.base))
        .bearer_auth("0123456789abcdef")
        .json(&request(
            1,
            Command::SessionList(SessionListParams {
                project_key: None,
                limit: 10,
            }),
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(authorized.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn server_profile_enforces_origin_and_token() {
    let options = ServeOptions {
        profile: ServeProfile::Server,
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        token: Some("0123456789abcdef".to_owned()),
        public_url: Some("https://cool.example.com".to_owned()),
        trust_proxy: true,
        ..ServeOptions::default()
    };
    let serving = start(durable_server(), options).await;
    let client = reqwest::Client::new();
    let body = request(
        1,
        Command::SessionList(SessionListParams {
            project_key: None,
            limit: 10,
        }),
    );

    // Missing Origin on a mutation fails closed in the server profile.
    let missing_origin = client
        .post(format!("{}/api/rpc", serving.base))
        .bearer_auth("0123456789abcdef")
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(missing_origin.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_origin = client
        .post(format!("{}/api/rpc", serving.base))
        .bearer_auth("0123456789abcdef")
        .header("origin", "https://evil.example.com")
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(wrong_origin.status(), reqwest::StatusCode::FORBIDDEN);

    let correct = client
        .post(format!("{}/api/rpc", serving.base))
        .bearer_auth("0123456789abcdef")
        .header("origin", "https://cool.example.com")
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(correct.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn server_profile_configuration_is_fail_closed() {
    let missing_token = HttpFacade::new(
        durable_server(),
        ServeOptions {
            profile: ServeProfile::Server,
            trust_proxy: true,
            public_url: Some("https://cool.example.com".to_owned()),
            ..ServeOptions::default()
        },
    );
    assert!(missing_token.is_err());
}

#[tokio::test]
async fn assets_serve_the_spa_bundle_with_fallback() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(directory.path().join("index.html"), "<h1>cool-spa</h1>").expect("index write");
    std::fs::write(directory.path().join("app.js"), "console.log('cool')").expect("asset write");
    let options = ServeOptions {
        assets: Some(directory.path().to_path_buf()),
        ..ServeOptions::default()
    };
    let serving = start(durable_server(), options).await;
    let client = reqwest::Client::new();

    let root = client
        .get(format!("{}/", serving.base))
        .send()
        .await
        .expect("root");
    assert_eq!(root.status(), reqwest::StatusCode::OK);
    assert!(root.text().await.expect("body").contains("cool-spa"));

    let asset = client
        .get(format!("{}/app.js", serving.base))
        .send()
        .await
        .expect("asset");
    assert_eq!(asset.status(), reqwest::StatusCode::OK);

    let deep = client
        .get(format!("{}/projects/42/settings", serving.base))
        .send()
        .await
        .expect("deep link");
    assert_eq!(deep.status(), reqwest::StatusCode::OK);
    assert!(deep.text().await.expect("body").contains("cool-spa"));
}

#[tokio::test]
async fn without_assets_root_returns_a_diagnostic_placeholder() {
    let serving = start(durable_server(), ServeOptions::default()).await;
    let response = reqwest::get(format!("{}/", serving.base))
        .await
        .expect("root");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.expect("body");
    assert!(body.contains("--assets"), "placeholder:\n{body}");
}
