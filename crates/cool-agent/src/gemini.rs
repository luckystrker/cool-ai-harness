//! Native Google Gemini `ModelDriver` (P2.11).
//!
//! Talks to the Generative Language API
//! (`POST {base}/v1beta/models/{model}:streamGenerateContent?alt=sse`)
//! directly. Wire differences this driver papers over:
//!
//! * Auth: API keys via the `x-goog-api-key` header; OAuth tokens (Gemini
//!   CLI / Cloud Code credentials) via `Authorization: Bearer`.
//! * The system prompt is a top-level `systemInstruction`, not a content.
//! * Tools are declared as `{functionDeclarations: [...]}`; calls and results
//!   are `functionCall` / `functionResponse` parts inside `contents` —
//!   Gemini has no tool-call ids, so call ids are synthesized
//!   (`gemini-call-{n}`) and `functionResponse` pairs by `name`.
//! * Streaming uses `data: {candidates:[{content:{parts:[...]}}],
//!   usageMetadata}` SSE chunks; `usageMetadata` maps onto `Usage`.
//!
//! Same hardening as the other drivers: DNS pinning through `NetworkPolicy`,
//! redirects disabled, bounded response bytes, 401 → `provider_unauthorized`
//! with one refresh + retry when an OAuth token source is attached.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use cool_security::NetworkPolicy;
use futures_util::{StreamExt as _, stream};
use reqwest::redirect::Policy;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use url::Url;

use crate::context::{Message, MessageRole, ModelContentPart, ToolCall};
use crate::pricing::estimate_cost_micro_usd;
use crate::provider::{
    AccessTokenSource, ModelDriver, ModelEvent, ModelRequest, ModelStream, ProviderError, Usage,
    decode_sse_lines, is_unauthorized,
};

/// Synthesized call-id prefix; Gemini function calls carry no id.
const CALL_ID_PREFIX: &str = "gemini-call-";

#[derive(Clone)]
pub struct GeminiDriver {
    base_url: Url,
    api_key: Option<String>,
    token_source: Option<Arc<dyn AccessTokenSource>>,
    network_policy: NetworkPolicy,
}

impl GeminiDriver {
    pub fn new(
        base_url: &str,
        api_key: impl Into<String>,
        network_policy: NetworkPolicy,
    ) -> Result<Self, ProviderError> {
        let normalized = if base_url.ends_with('/') {
            base_url.to_owned()
        } else {
            format!("{base_url}/")
        };
        let base_url = Url::parse(&normalized)
            .map_err(|error| ProviderError::new("invalid_base_url", error.to_string(), false))?;
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(ProviderError::new(
                "provider_credentials_missing",
                "Gemini requires an API key or OAuth login",
                false,
            ));
        }
        Ok(Self {
            base_url,
            api_key: Some(api_key),
            token_source: None,
            network_policy,
        })
    }

    /// OAuth-backed driver (Gemini CLI credential): bearer token from the
    /// source; a 401 triggers one refresh + retry.
    pub fn for_oauth(
        base_url: &str,
        token_source: Arc<dyn AccessTokenSource>,
        network_policy: NetworkPolicy,
    ) -> Result<Self, ProviderError> {
        let normalized = if base_url.ends_with('/') {
            base_url.to_owned()
        } else {
            format!("{base_url}/")
        };
        let base_url = Url::parse(&normalized)
            .map_err(|error| ProviderError::new("invalid_base_url", error.to_string(), false))?;
        Ok(Self {
            base_url,
            api_key: None,
            token_source: Some(token_source),
            network_policy,
        })
    }

    async fn credential(&self) -> Result<String, ProviderError> {
        match &self.token_source {
            Some(source) => source.access_token().await,
            None => Ok(self.api_key.clone().unwrap_or_default()),
        }
    }
}

#[async_trait]
impl ModelDriver for GeminiDriver {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let credential = self.credential().await?;
        match self.stream_once(request.clone(), credential).await {
            Err(error) if is_unauthorized(&error) && self.token_source.is_some() => {
                let source = self.token_source.as_ref().expect("checked above");
                let refreshed = source.refresh().await?;
                self.stream_once(request, refreshed).await
            }
            result => result,
        }
    }
}

impl GeminiDriver {
    async fn stream_once(
        &self,
        request: ModelRequest,
        credential: String,
    ) -> Result<ModelStream, ProviderError> {
        let url = self
            .base_url
            .join(&format!(
                "v1beta/models/{}:streamGenerateContent?alt=sse",
                request.model
            ))
            .map_err(|error| {
                ProviderError::new("invalid_provider_url", error.to_string(), false)
            })?;
        let host = url
            .host_str()
            .ok_or_else(|| ProviderError::new("invalid_provider_url", "missing host", false))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| ProviderError::new("invalid_provider_url", "missing port", false))?;
        let resolved = tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| ProviderError::new("provider_dns", error.to_string(), true))?
            .collect::<Vec<SocketAddr>>();
        let pinned = self
            .network_policy
            .pin(url.as_str(), resolved.iter().map(SocketAddr::ip))
            .map_err(|error| {
                ProviderError::new("provider_network_denied", error.to_string(), false)
            })?;
        let address = resolved
            .iter()
            .find(|candidate| pinned.addresses.contains(&candidate.ip()))
            .copied()
            .ok_or_else(|| ProviderError::new("provider_dns", "no pinned socket", false))?;
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(self.network_policy.timeout)
            .resolve(host, address)
            .build()
            .map_err(|error| ProviderError::new("provider_client", error.to_string(), false))?;
        let model = request.model.clone();
        let payload = gemini_payload(&request);
        let mut request_builder = client.post(url).json(&payload);
        if self.token_source.is_some() {
            request_builder = request_builder.bearer_auth(&credential);
        } else {
            request_builder = request_builder.header("x-goog-api-key", &credential);
        }
        let response = request_builder
            .send()
            .await
            .map_err(|error| ProviderError::new("provider_request", error.to_string(), true))?;
        if response.status().is_redirection() {
            return Err(ProviderError::new(
                "provider_redirect_denied",
                "provider redirects require explicit revalidation",
                false,
            ));
        }
        if !response.status().is_success() {
            let status = response.status();
            let retryable = status.as_u16() == 429 || status.is_server_error();
            return Err(ProviderError::new(
                if status.as_u16() == 401 {
                    "provider_unauthorized"
                } else {
                    "provider_http"
                },
                format!("provider returned {status}"),
                retryable,
            ));
        }
        let max_bytes = self.network_policy.max_response_bytes;
        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            let mut body = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut received = 0_u64;
            let mut calls = 0_u64;
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = sender
                            .send(Err(ProviderError::new(
                                "provider_stream",
                                error.to_string(),
                                true,
                            )))
                            .await;
                        return;
                    }
                };
                received += chunk.len() as u64;
                if received > max_bytes {
                    let _ = sender
                        .send(Err(ProviderError::new(
                            "provider_response_too_large",
                            "provider stream exceeded configured limit",
                            false,
                        )))
                        .await;
                    return;
                }
                let lines = match decode_sse_lines(&mut buffer, &chunk) {
                    Ok(lines) => lines,
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                        return;
                    }
                };
                for line in lines {
                    match process_sse_line(&line, &sender, &mut calls, &model).await {
                        Ok(()) => {}
                        Err(error) => {
                            let _ = sender.send(Err(error)).await;
                            return;
                        }
                    }
                }
            }
            if !buffer.is_empty() {
                let _ = sender
                    .send(Err(ProviderError::new(
                        "provider_stream_incomplete",
                        "provider stream ended inside an SSE line",
                        true,
                    )))
                    .await;
            }
        });
        Ok(Box::pin(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })))
    }
}

/// Canonical messages → Gemini `{contents, systemInstruction, tools,
/// generationConfig}`.
fn gemini_payload(request: &ModelRequest) -> Value {
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if let Some(text) = message.content.as_ref().filter(|text| !text.is_empty()) {
                    system_parts.push(json!({"text": text}));
                }
            }
            MessageRole::Tool => contents.push(tool_content(message)),
            _ => contents.push(gemini_content(message)),
        }
    }
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({"name": tool.name, "description": tool.description, "parameters": tool.parameters})
        })
        .collect::<Vec<_>>();
    let mut generation = Map::new();
    generation.insert("temperature".to_owned(), json!(request.temperature));
    if let Some(max_tokens) = request.max_tokens {
        generation.insert("maxOutputTokens".to_owned(), json!(max_tokens));
    }
    let mut payload = json!({
        "contents": contents,
        "generationConfig": Value::Object(generation),
    });
    if !system_parts.is_empty() {
        payload["systemInstruction"] = json!({"parts": system_parts});
    }
    if !tools.is_empty() {
        payload["tools"] = json!([{"functionDeclarations": tools}]);
    }
    payload
}

/// One non-tool message → `{role, parts}`; `Text`/`Image` model parts append
/// after the text part so vision input reaches Gemini as `inlineData`.
fn gemini_content(message: &Message) -> Value {
    let role = match message.role {
        MessageRole::Assistant => "model",
        MessageRole::User | MessageRole::System | MessageRole::Tool => "user",
    };
    let mut parts = Vec::new();
    if let Some(text) = message.content.as_ref().filter(|text| !text.is_empty()) {
        parts.push(json!({"text": text}));
    }
    for call in &message.tool_calls {
        parts.push(json!({
            "functionCall": {"name": call.name, "args": Value::Object(call.arguments.clone())},
        }));
    }
    if let Some(model_parts) = message.parts.as_deref() {
        for part in model_parts {
            match part {
                ModelContentPart::Text { text } => parts.push(json!({"text": text})),
                ModelContentPart::Image {
                    media_type,
                    data_base64,
                } => parts.push(json!({
                    "inlineData": {"mimeType": media_type, "data": data_base64}
                })),
            }
        }
    }
    if parts.is_empty() {
        parts.push(json!({"text": ""}));
    }
    json!({"role": role, "parts": parts})
}

/// Tool results become `functionResponse` parts on a user-role content; the
/// JSON output is nested under `result` to satisfy the `object` shape.
fn tool_content(message: &Message) -> Value {
    let name = message.name.clone().unwrap_or_default();
    let response = message
        .content
        .as_deref()
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .map(|mut parsed| {
            if !parsed.is_object() {
                parsed = json!({"result": parsed});
            }
            parsed
        })
        .unwrap_or_else(|| json!({"result": message.content.clone().unwrap_or_default()}));
    json!({
        "role": "user",
        "parts": [{"functionResponse": {"name": name, "response": response}}],
    })
}

/// One SSE `data:` line → zero or more `ModelEvent`s. `calls` mints
/// `gemini-call-{n}` ids for function calls, which carry no provider id.
async fn process_sse_line(
    line: &str,
    sender: &mpsc::Sender<Result<ModelEvent, ProviderError>>,
    calls: &mut u64,
    model: &str,
) -> Result<(), ProviderError> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|error| ProviderError::new("provider_sse", error.to_string(), false))?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("provider stream error");
        let code = error
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_lowercase();
        let retryable = matches!(code.as_str(), "unavailable" | "resource_exhausted");
        return Err(ProviderError::new(
            format!("provider_{code}"),
            message.to_owned(),
            retryable,
        ));
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_array)
        && let Some(candidate) = candidates.first()
    {
        if let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let _ = sender.send(Ok(ModelEvent::Content(text.to_owned()))).await;
                } else if let Some(call) = part.get("functionCall") {
                    *calls += 1;
                    let arguments = call
                        .get("args")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let _ = sender
                        .send(Ok(ModelEvent::ToolCall(ToolCall {
                            call_id: format!("{CALL_ID_PREFIX}{calls}"),
                            name: call
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            arguments,
                        })))
                        .await;
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str)
            && reason != "FINISH_REASON_UNSPECIFIED"
        {
            let mapped = match reason {
                "STOP" => "stop",
                "MAX_TOKENS" => "max_tokens",
                other => other,
            };
            let _ = sender
                .send(Ok(ModelEvent::Finish {
                    reason: Some(mapped.to_owned()),
                }))
                .await;
        }
    }
    if let Some(usage) = value.get("usageMetadata") {
        let prompt_tokens = usage
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let completion_tokens = usage
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let total_tokens = usage
            .get("totalTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(prompt_tokens + completion_tokens);
        let cost_micro_usd = estimate_cost_micro_usd(model, prompt_tokens, completion_tokens, 0, 0);
        let _ = sender
            .send(Ok(ModelEvent::Usage(Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_tokens,
                cost_micro_usd,
            })))
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "gemini-2.5-flash".to_owned(),
            messages,
            tools: vec![crate::tools::ToolDefinition {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                parameters: json!({"type": "object"}),
            }],
            temperature: 0.5,
            max_tokens: Some(64),
        }
    }

    #[test]
    fn payload_maps_roles_parts_and_tools() {
        let mut assistant = Message::text(MessageRole::Assistant, "checking");
        assistant.tool_calls.push(ToolCall {
            call_id: "call_1".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::Map::from_iter([("path".to_owned(), json!("a.txt"))]),
        });
        let tool = Message::tool_result(&assistant.tool_calls[0], "{\"size\": 5}");
        let payload = gemini_payload(&request(vec![
            Message::text(MessageRole::System, "be brief"),
            Message::with_parts(
                MessageRole::User,
                "look\n[image: artifact-1]",
                vec![ModelContentPart::Image {
                    media_type: "image/png".to_owned(),
                    data_base64: "aGk=".to_owned(),
                }],
            ),
            assistant,
            tool,
        ]));
        assert_eq!(payload["systemInstruction"]["parts"][0]["text"], "be brief");
        let contents = payload["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "look\n[image: artifact-1]");
        assert_eq!(
            contents[0]["parts"][1]["inlineData"],
            json!({"mimeType": "image/png", "data": "aGk="})
        );
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(
            contents[1]["parts"][1]["functionCall"],
            json!({"name": "read_file", "args": {"path": "a.txt"}})
        );
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"],
            json!({"name": "read_file", "response": {"size": 5}})
        );
        assert_eq!(
            payload["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
        assert_eq!(payload["generationConfig"]["maxOutputTokens"], 64);
    }

    #[tokio::test]
    async fn sse_stream_parses_text_calls_and_usage() {
        let (sender, mut receiver) = mpsc::channel(16);
        let mut calls = 0_u64;
        process_sse_line(
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"hi"},{"functionCall":{"name":"read_file","args":{"path":"a.txt"}}}]}}]}"#,
            &sender,
            &mut calls,
            "gemini-2.5-flash",
        )
        .await
        .unwrap();
        process_sse_line(
            r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":4,"totalTokenCount":14}}"#,
            &sender,
            &mut calls,
            "gemini-2.5-flash",
        )
        .await
        .unwrap();
        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Content("hi".to_owned())
        );
        let ModelEvent::ToolCall(call) = receiver.recv().await.unwrap().unwrap() else {
            panic!("expected tool call")
        };
        assert_eq!(call.call_id, "gemini-call-1");
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments["path"], "a.txt");
        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Content("done".to_owned())
        );
        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Finish {
                reason: Some("stop".to_owned())
            }
        );
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Usage(Usage {
                prompt_tokens: 10,
                completion_tokens: 4,
                total_tokens: 14,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn sse_error_maps_to_provider_error() {
        let (sender, _receiver) = mpsc::channel(1);
        let mut calls = 0_u64;
        let error = process_sse_line(
            r#"data: {"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#,
            &sender,
            &mut calls,
            "m",
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "provider_unauthenticated");
        assert!(!error.retryable);
    }

    /// End-to-end stream over a local HTTP server: 401 once, refresh, then a
    /// successful SSE body — the P2.11 refresh-and-retry contract.
    #[tokio::test]
    async fn oauth_stream_refreshes_once_on_401() {
        use std::sync::Mutex as StdMutex;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        struct StubSource {
            refresh_calls: AtomicUsize,
        }
        impl std::fmt::Debug for StubSource {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("StubSource").finish()
            }
        }
        #[async_trait]
        impl AccessTokenSource for StubSource {
            async fn access_token(&self) -> Result<String, ProviderError> {
                Ok("stale-token".to_owned())
            }
            async fn refresh(&self) -> Result<String, ProviderError> {
                self.refresh_calls.fetch_add(1, Ordering::SeqCst);
                Ok("fresh-token".to_owned())
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(StdMutex::new(Vec::<String>::new()));
        let seen_server = Arc::clone(&seen);
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let count = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                let auth = request
                    .lines()
                    .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or_default()
                    .to_owned();
                seen_server.lock().unwrap().push(auth);
                let (status, body) = if request.contains("stale-token") {
                    ("HTTP/1.1 401 Unauthorized", "")
                } else {
                    (
                        "HTTP/1.1 200 OK",
                        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n",
                    )
                };
                let response = format!(
                    "{status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let source = Arc::new(StubSource {
            refresh_calls: AtomicUsize::new(0),
        });
        let driver = GeminiDriver::for_oauth(
            &format!("http://{address}/"),
            source.clone() as Arc<dyn AccessTokenSource>,
            NetworkPolicy::new([address.ip().to_string()]).loopback_only(),
        )
        .unwrap();
        let mut stream = driver
            .stream(request(vec![Message::text(MessageRole::User, "hello")]))
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        server.await.unwrap();
        assert_eq!(source.refresh_calls.load(Ordering::SeqCst), 1);
        let auths = seen.lock().unwrap();
        assert!(auths[0].contains("Bearer stale-token"), "{}", auths[0]);
        assert!(auths[1].contains("Bearer fresh-token"), "{}", auths[1]);
        assert!(events.contains(&ModelEvent::Content("ok".to_owned())));
    }
}
