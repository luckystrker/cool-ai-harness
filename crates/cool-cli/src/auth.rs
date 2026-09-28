//! `cool auth <claude|chatgpt|gemini>` — provider OAuth login (P2.11).
//!
//! Runs entirely in the CLI: loopback listener (or Claude's manual
//! paste-the-code page, or Codex's device flow), PKCE exchange through
//! `cool_app_server::oauth`, then tokens land Fernet-encrypted on the
//! provider row via `set_provider_oauth_tokens`.

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use cool_app_server::oauth::{self, OAuthFlow, OAuthTokens, PendingOAuth, oauth_flow};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{configured_secrets, open_legacy_store, runtime};

/// `cool auth <provider> [--data-dir DIR] [--device] [--manual]`.
pub async fn auth_command(
    provider: &str,
    data_dir: &Path,
    device: bool,
    manual: bool,
) -> Result<(), (i32, serde_json::Value)> {
    let Some(flow) = oauth_flow(provider) else {
        return Err(runtime(
            "oauth_provider_unsupported",
            "no verified OAuth flow for this provider (supported: claude, chatgpt, gemini)",
        ));
    };
    oauth::flow_ready(&flow).map_err(|error| runtime(error.code, &error.message))?;
    let store = open_legacy_store(&data_dir.join("harness.db"))?;
    let Some(secrets) = configured_secrets() else {
        return Err(runtime(
            "oauth_secrets_unavailable",
            "SECRET_KEY is required — OAuth tokens are Fernet-encrypted before they reach the store",
        ));
    };
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| runtime("oauth_http_client", &error.to_string()))?;
    let tokens = if device {
        device_login(&http, flow.clone()).await?
    } else if manual || flow.manual_redirect.is_some() {
        manual_login(&http, flow.clone()).await?
    } else {
        loopback_login(&http, flow.clone()).await?
    };
    let actor = "local-user";
    let provider_row = oauth::oauth_provider_row(&store, actor, flow.clone())
        .map_err(|error| runtime("oauth_store_failed", &error.to_string()))?;
    let encrypted = oauth::encrypt_tokens(&secrets, &tokens)
        .map_err(|error| runtime(error.code, &error.message))?;
    store
        .set_provider_oauth_tokens(actor, provider_row.id, &encrypted)
        .map_err(|error| runtime("oauth_store_failed", &error.to_string()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": true,
            "provider": flow.name,
            "providerId": provider_row.id,
            "providerName": provider_row.name,
            "expiresAt": tokens.expires_at,
            "notice": match flow.name {
                "claude" => "Anthropic subscription OAuth is off-label use; an API key remains the supported credential.",
                "chatgpt" => "Codex tokens authenticate OpenAI's Codex backend; the chat/completions driver reports oauth_wire_not_supported.",
                _ => "Stored. COOL_PROVIDER selects the OAuth-backed driver.",
            },
        }))
        .expect("auth result serializes")
    );
    Ok(())
}

/// Loopback PKCE login: bind the provider's callback port (or an ephemeral
/// one for Google), print the authorize URL, accept the `GET` redirect.
async fn loopback_login(
    http: &reqwest::Client,
    flow: OAuthFlow,
) -> Result<OAuthTokens, (i32, serde_json::Value)> {
    let pkce = oauth::pkce_pair();
    let state = flow.state(&pkce);
    let (listener, redirect_uri) = match flow.loopback_port {
        Some(port) => {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .map_err(|error| {
                    runtime(
                        "oauth_loopback_bind_failed",
                        &format!("could not bind 127.0.0.1:{port}: {error}"),
                    )
                })?;
            (
                listener,
                format!("http://localhost:{port}{}", flow.loopback_path),
            )
        }
        None => {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|error| runtime("oauth_loopback_bind_failed", &error.to_string()))?;
            let port = listener
                .local_addr()
                .map_err(|error| runtime("oauth_loopback_bind_failed", &error.to_string()))?
                .port();
            (
                listener,
                format!("http://127.0.0.1:{port}{}", flow.loopback_path),
            )
        }
    };
    let url = oauth::authorize_url(flow.clone(), &redirect_uri, &pkce.challenge, &state);
    eprintln!("Open this URL in your browser to sign in:\n\n{url}\n");
    eprintln!("Waiting for the {redirect_uri} callback (5 minute timeout)…");
    let code = tokio::time::timeout(
        Duration::from_secs(300),
        accept_callback(&listener, &state, flow.loopback_path),
    )
    .await
    .map_err(|_| runtime("oauth_timeout", "no callback arrived within 5 minutes"))??;
    exchange(http, flow, pkce.verifier, redirect_uri, &code).await
}

/// One loopback accept loop: favicon/health probes get a 404; the real
/// `GET {path}?code=…&state=…` gets a 200 page and ends the flow.
async fn accept_callback(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
    path: &str,
) -> Result<String, (i32, serde_json::Value)> {
    for _ in 0..16 {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|error| runtime("oauth_loopback_accept", &error.to_string()))?;
        let mut buffer = vec![0_u8; 16 * 1024];
        let count = socket
            .read(&mut buffer)
            .await
            .map_err(|error| runtime("oauth_loopback_accept", &error.to_string()))?;
        let request = String::from_utf8_lossy(&buffer[..count]);
        let Some(request_line) = request.lines().next() else {
            continue;
        };
        let Some(target) = request_line.split_whitespace().nth(1) else {
            continue;
        };
        let Ok(parsed) = url::Url::parse(&format!("http://localhost{target}")) else {
            continue;
        };
        if parsed.path() != path {
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        }
        let mut code = None;
        let mut state = None;
        let mut oauth_error = None;
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => oauth_error = Some(value.into_owned()),
                _ => {}
            }
        }
        if let Some(error) = oauth_error {
            respond(
                &mut socket,
                "HTTP/1.1 200 OK",
                &format!("<p>Authorization failed: {error}. You can close this tab.</p>"),
            )
            .await;
            return Err(runtime(
                "oauth_denied",
                &format!("provider returned error: {error}"),
            ));
        }
        let Some(code) = code.filter(|code| !code.is_empty()) else {
            respond(
                &mut socket,
                "HTTP/1.1 400 Bad Request",
                "<p>Missing <code>code</code> — return to the CLI and retry.</p>",
            )
            .await;
            continue;
        };
        if state.as_deref() != Some(expected_state) {
            respond(
                &mut socket,
                "HTTP/1.1 400 Bad Request",
                "<p>State mismatch — return to the CLI and retry.</p>",
            )
            .await;
            return Err(runtime(
                "oauth_state_mismatch",
                "callback state does not match the started handshake",
            ));
        }
        respond(
            &mut socket,
            "HTTP/1.1 200 OK",
            "<p>Login complete — you can close this tab and return to the CLI.</p>",
        )
        .await;
        return Ok(code);
    }
    Err(runtime(
        "oauth_loopback_accept",
        "callback listener received no authorization code",
    ))
}

/// Write one HTTP response to a callback connection.
async fn respond(socket: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "{status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

/// Claude's verified path: Anthropic's console shows `code#state` on the
/// `oauth/code/callback` page; the user pastes it into the terminal.
async fn manual_login(
    http: &reqwest::Client,
    flow: OAuthFlow,
) -> Result<OAuthTokens, (i32, serde_json::Value)> {
    let redirect_uri = flow.manual_redirect.ok_or_else(|| {
        runtime(
            "oauth_manual_unsupported",
            "this provider has no manual paste-the-code flow — use loopback",
        )
    })?;
    let pkce = oauth::pkce_pair();
    let state = flow.state(&pkce);
    let url = oauth::authorize_url(flow.clone(), redirect_uri, &pkce.challenge, &state);
    eprintln!("Open this URL in your browser to sign in:\n\n{url}\n");
    eprint!("Paste the code shown after login (the full `code#state` is fine): ");
    std::io::stdout()
        .flush()
        .map_err(|error| runtime("oauth_stdin", &error.to_string()))?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await
    .map_err(|error| runtime("oauth_stdin", &error.to_string()))?
    .map_err(|error| runtime("oauth_stdin", &error.to_string()))?;
    let code = line
        .trim()
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if code.is_empty() {
        return Err(runtime("oauth_code_missing", "empty code pasted"));
    }
    exchange(http, flow, pkce.verifier, redirect_uri.to_owned(), &code).await
}

/// Codex device authorization: print the code + URL, poll until issued.
async fn device_login(
    http: &reqwest::Client,
    flow: OAuthFlow,
) -> Result<OAuthTokens, (i32, serde_json::Value)> {
    let device = oauth::device_user_code(http, flow.clone())
        .await
        .map_err(|error| runtime(error.code, &error.message))?;
    eprintln!(
        "Open {} in your browser and enter code: {}\n",
        device.verification_url, device.user_code
    );
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    loop {
        tokio::time::sleep(Duration::from_secs(device.interval_secs.max(3))).await;
        match oauth::device_poll_once(http, flow.clone(), &device)
            .await
            .map_err(|error| runtime(error.code, &error.message))?
        {
            Some(tokens) => return Ok(tokens),
            None if Instant::now() < deadline => {}
            None => {
                return Err(runtime(
                    "oauth_timeout",
                    "device authorization timed out after 15 minutes",
                ));
            }
        }
    }
}

async fn exchange(
    http: &reqwest::Client,
    flow: OAuthFlow,
    verifier: String,
    redirect_uri: String,
    code: &str,
) -> Result<OAuthTokens, (i32, serde_json::Value)> {
    let pending = PendingOAuth {
        flow: flow.clone(),
        verifier,
        redirect_uri,
        created: Instant::now(),
    };
    oauth::exchange_code(http, flow, &pending, code)
        .await
        .map_err(|error| runtime(error.code, &error.message))
}
