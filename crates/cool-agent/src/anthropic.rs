//! Native Anthropic (Claude) `ModelDriver`.
//!
//! Parity with `backend/app/providers/anthropic.py`: talks to Anthropic's
//! Messages API (`POST {base}/v1/messages`) directly instead of an
//! OpenAI-compatible proxy. Wire differences this driver papers over:
//!
//! * Auth via `x-api-key` + `anthropic-version` headers (not Bearer).
//! * The system prompt is a top-level `system` field, not a chat message.
//! * `max_tokens` is required on every request.
//! * Tools are declared as `{name, description, input_schema}`, and tool
//!   calls/results are `tool_use` / `tool_result` content blocks.
//! * Streaming uses typed SSE events (`content_block_delta`, …) rather than
//!   OpenAI's `data: {choices: [...]}` chunks.
//!
//! Same hardening as the OpenAI driver: DNS pinning through `NetworkPolicy`,
//! redirects disabled, bounded response bytes.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use cool_security::NetworkPolicy;
use futures_util::{StreamExt as _, stream};
use reqwest::redirect::Policy;
use serde_json::{Value, json};
use url::Url;

use crate::context::{Message, MessageRole, ModelContentPart, ToolCall};
use crate::pricing::estimate_cost_micro_usd;
use crate::provider::{
    ModelDriver, ModelEvent, ModelRequest, ModelStream, ProviderError, Usage, decode_sse_lines,
};

/// Anthropic requires `max_tokens`; fall back to the Python provider's
/// default when the request does not specify one.
const DEFAULT_MAX_TOKENS: u32 = 4096;
const API_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct AnthropicDriver {
    base_url: Url,
    api_key: Option<String>,
    /// OAuth bearer source (P2.11): set for Claude subscription logins, where
    /// the credential rides `Authorization: Bearer` + `anthropic-beta:
    /// oauth-2025-04-20` instead of `x-api-key`.
    token_source: Option<Arc<dyn crate::provider::AccessTokenSource>>,
    network_policy: NetworkPolicy,
}

impl AnthropicDriver {
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
                "Anthropic requires an API key",
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

    /// OAuth-backed driver (P2.11): bearer token from the source; a 401
    /// triggers one refresh + retry.
    pub fn for_oauth(
        base_url: &str,
        token_source: Arc<dyn crate::provider::AccessTokenSource>,
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
impl ModelDriver for AnthropicDriver {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let credential = self.credential().await?;
        match self.stream_once(request.clone(), credential).await {
            Err(error)
                if crate::provider::is_unauthorized(&error) && self.token_source.is_some() =>
            {
                let source = self.token_source.as_ref().expect("checked above");
                let refreshed = source.refresh().await?;
                self.stream_once(request, refreshed).await
            }
            result => result,
        }
    }
}

impl AnthropicDriver {
    async fn stream_once(
        &self,
        request: ModelRequest,
        credential: String,
    ) -> Result<ModelStream, ProviderError> {
        let url = self.base_url.join("v1/messages").map_err(|error| {
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
        let payload = anthropic_payload(&request);
        let mut request_builder = client.post(url).header("anthropic-version", API_VERSION);
        if self.token_source.is_some() {
            // OAuth access tokens ride Bearer + the oauth beta flag (Claude
            // Code's subscription flow); API keys keep x-api-key.
            request_builder = request_builder
                .bearer_auth(&credential)
                .header("anthropic-beta", "oauth-2025-04-20");
        } else {
            request_builder = request_builder.header("x-api-key", &credential);
        }
        let response = request_builder
            .json(&payload)
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
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            let mut body = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut received = 0_u64;
            let mut state = AnthropicStreamState::new(model);
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
                    match state.process_line(&line, &sender).await {
                        Ok(true) => return,
                        Ok(false) => {}
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
                return;
            }
            state.finish(&sender).await;
        });
        Ok(Box::pin(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })))
    }
}

/// Canonical harness messages → Anthropic `{model, system, messages, tools}`.
fn anthropic_payload(request: &ModelRequest) -> Value {
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        if message.role == MessageRole::System {
            if let Some(content) = message.content.as_ref().filter(|text| !text.is_empty()) {
                system_parts.push(content.clone());
            }
            continue;
        }
        messages.push(anthropic_message(message));
    }
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({"name": tool.name, "description": tool.description, "input_schema": tool.parameters})
        })
        .collect::<Vec<_>>();
    let mut payload = json!({
        "model": request.model,
        "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "temperature": request.temperature,
        "stream": true,
        "messages": messages,
    });
    if !system_parts.is_empty() {
        payload["system"] = Value::String(system_parts.join("\n\n"));
    }
    if !tools.is_empty() {
        payload["tools"] = Value::Array(tools);
    }
    payload
}

/// Convert one canonical message into Anthropic's `{role, content: [blocks]}`.
/// Tool results become a user-role `tool_result` block; assistant tool calls
/// become `tool_use` blocks appended after any text.
fn anthropic_message(message: &Message) -> Value {
    if message.role == MessageRole::Tool {
        // `tool_result.content` may be a string or a block array — vision
        // parts (`view_image`) travel as image blocks (P2.12).
        let content = match message.parts.as_deref().filter(|parts| !parts.is_empty()) {
            Some(parts) => {
                let mut blocks = Vec::new();
                if let Some(text) = message.content.as_ref().filter(|text| !text.is_empty()) {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for block in parts.iter().filter_map(anthropic_part_block) {
                    blocks.push(block);
                }
                Value::Array(blocks)
            }
            None => message
                .content
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        };
        return json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": message.tool_call_id,
                "content": content,
            }],
        });
    }
    let role = match message.role {
        MessageRole::Assistant => "assistant",
        MessageRole::System | MessageRole::User | MessageRole::Tool => "user",
    };
    let mut blocks = Vec::new();
    if let Some(content) = message.content.as_ref().filter(|text| !text.is_empty()) {
        blocks.push(json!({"type": "text", "text": content}));
    }
    for call in &message.tool_calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": call.call_id,
            "name": call.name,
            "input": Value::Object(call.arguments.clone()),
        }));
    }
    if let Some(parts) = message.parts.as_deref().filter(|parts| !parts.is_empty()) {
        for block in parts.iter().filter_map(anthropic_part_block) {
            blocks.push(block);
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": ""}));
    }
    json!({"role": role, "content": blocks})
}

/// One `ModelContentPart` → an Anthropic `{type:"text"|"image"}` block.
fn anthropic_part_block(part: &ModelContentPart) -> Option<Value> {
    match part {
        ModelContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
        ModelContentPart::Image {
            media_type,
            data_base64,
        } => Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data_base64}
        })),
    }
}

#[derive(Default)]
struct AnthropicToolBlock {
    id: String,
    name: String,
    input_json: String,
}

struct AnthropicStreamState {
    model: String,
    tool_blocks: BTreeMap<u64, AnthropicToolBlock>,
    /// `message_start` carries `input_tokens` plus the cache fields;
    /// `message_delta` carries the cumulative `output_tokens` and may repeat
    /// or omit the rest.
    start_input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    finish_reason: Option<String>,
}

impl AnthropicStreamState {
    fn new(model: String) -> Self {
        Self {
            model,
            tool_blocks: BTreeMap::new(),
            start_input_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            finish_reason: None,
        }
    }

    /// Anthropic counts cached tokens separately from `input_tokens`:
    /// `cache_creation_input_tokens` are written at a premium and
    /// `cache_read_input_tokens` are billed at a discount — track both so the
    /// run budget sees the real cost, not just the raw input bill.
    fn track_cache(&mut self, usage: &Value) {
        self.cache_read_tokens = usage["cache_read_input_tokens"]
            .as_u64()
            .unwrap_or(self.cache_read_tokens);
        self.cache_write_tokens = usage["cache_creation_input_tokens"]
            .as_u64()
            .unwrap_or(self.cache_write_tokens);
    }

    async fn process_line(
        &mut self,
        line: &str,
        sender: &tokio::sync::mpsc::Sender<Result<ModelEvent, ProviderError>>,
    ) -> Result<bool, ProviderError> {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(false);
        };
        if data.is_empty() {
            return Ok(false);
        }
        let event: Value = serde_json::from_str(data)
            .map_err(|error| ProviderError::new("provider_json", error.to_string(), false))?;
        match event["type"].as_str() {
            Some("message_start") => {
                let usage = &event["message"]["usage"];
                self.start_input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
                self.track_cache(usage);
            }
            Some("content_block_start") => {
                let index = event["index"].as_u64().unwrap_or(0);
                let block = &event["content_block"];
                if block["type"].as_str() == Some("tool_use") {
                    self.tool_blocks.insert(
                        index,
                        AnthropicToolBlock {
                            id: block["id"].as_str().unwrap_or_default().to_owned(),
                            name: block["name"].as_str().unwrap_or_default().to_owned(),
                            input_json: String::new(),
                        },
                    );
                }
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = delta["text"].as_str().filter(|text| !text.is_empty()) {
                            sender
                                .send(Ok(ModelEvent::Content(text.to_owned())))
                                .await
                                .ok();
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) =
                            delta["thinking"].as_str().filter(|text| !text.is_empty())
                        {
                            sender
                                .send(Ok(ModelEvent::Reasoning(thinking.to_owned())))
                                .await
                                .ok();
                        }
                    }
                    Some("input_json_delta") => {
                        let index = event["index"].as_u64().unwrap_or(0);
                        if let Some(block) = self.tool_blocks.get_mut(&index)
                            && let Some(partial) = delta["partial_json"].as_str()
                        {
                            block.input_json.push_str(partial);
                        }
                    }
                    // signature_delta and any newer delta kinds carry no content.
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = event["index"].as_u64().unwrap_or(0);
                if let Some(block) = self.tool_blocks.remove(&index) {
                    sender.send(block.finish()).await.ok();
                }
            }
            Some("message_delta") => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.finish_reason = Some(reason.to_owned());
                }
                let usage = &event["usage"];
                if !usage.is_null() {
                    self.track_cache(usage);
                    let prompt_tokens = usage["input_tokens"]
                        .as_u64()
                        .unwrap_or(self.start_input_tokens);
                    let completion_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
                    sender
                        .send(Ok(ModelEvent::Usage(Usage {
                            prompt_tokens,
                            completion_tokens,
                            cache_read_tokens: self.cache_read_tokens,
                            cache_write_tokens: self.cache_write_tokens,
                            total_tokens: prompt_tokens
                                + completion_tokens
                                + self.cache_read_tokens
                                + self.cache_write_tokens,
                            cost_micro_usd: estimate_cost_micro_usd(
                                &self.model,
                                prompt_tokens,
                                completion_tokens,
                                self.cache_read_tokens,
                                self.cache_write_tokens,
                            ),
                        })))
                        .await
                        .ok();
                }
            }
            Some("message_stop") => {
                // Flush any tool blocks still buffered — content_block_stop
                // is not guaranteed before the stream terminates.
                for (_, block) in std::mem::take(&mut self.tool_blocks) {
                    sender.send(block.finish()).await.ok();
                }
                sender
                    .send(Ok(ModelEvent::Finish {
                        reason: self.finish_reason.take(),
                    }))
                    .await
                    .ok();
                return Ok(true);
            }
            Some("error") => {
                let message = event["error"]["message"]
                    .as_str()
                    .unwrap_or("anthropic stream error")
                    .to_owned();
                let code = event["error"]["type"].as_str().unwrap_or("provider_error");
                return Err(ProviderError::new(
                    format!("provider_{code}"),
                    message,
                    // Overloaded/rate-limit style errors may succeed on retry.
                    matches!(code, "overloaded_error" | "rate_limit_error"),
                ));
            }
            // ping and unknown event types carry no payload.
            _ => {}
        }
        Ok(false)
    }

    /// Stream ended without `message_stop`: flush any tool blocks and finish
    /// so the caller never hangs waiting for a terminal event.
    async fn finish(
        &mut self,
        sender: &tokio::sync::mpsc::Sender<Result<ModelEvent, ProviderError>>,
    ) {
        for (_, block) in std::mem::take(&mut self.tool_blocks) {
            sender.send(block.finish()).await.ok();
        }
        sender
            .send(Ok(ModelEvent::Finish {
                reason: self.finish_reason.take(),
            }))
            .await
            .ok();
    }
}

impl AnthropicToolBlock {
    fn finish(self) -> Result<ModelEvent, ProviderError> {
        let arguments = if self.input_json.is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(&self.input_json).map_err(|error| {
                ProviderError::new("invalid_tool_arguments", error.to_string(), false)
            })?
        };
        Ok(ModelEvent::ToolCall(ToolCall {
            call_id: self.id,
            name: self.name,
            arguments,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolDefinition;

    fn message(role: MessageRole, content: Option<&str>) -> Message {
        Message {
            role,
            content: content.map(str::to_owned),
            parts: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "claude-sonnet-4-5".to_owned(),
            messages,
            tools: vec![ToolDefinition {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                parameters: json!({"type": "object"}),
            }],
            temperature: 0.7,
            max_tokens: None,
        }
    }

    #[test]
    fn payload_extracts_system_and_blocks() {
        let mut assistant = message(MessageRole::Assistant, Some("checking"));
        assistant.tool_calls.push(ToolCall {
            call_id: "call_1".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::Map::from_iter([("path".to_owned(), json!("a.txt"))]),
        });
        let mut tool = message(MessageRole::Tool, Some("contents"));
        tool.tool_call_id = Some("call_1".to_owned());
        let payload = anthropic_payload(&request(vec![
            message(MessageRole::System, Some("be brief")),
            message(MessageRole::User, Some("hi")),
            assistant,
            tool,
        ]));
        assert_eq!(payload["system"], "be brief");
        assert_eq!(payload["max_tokens"], DEFAULT_MAX_TOKENS);
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");
        let assistant_blocks = messages[1]["content"].as_array().unwrap();
        assert_eq!(assistant_blocks[0]["type"], "text");
        assert_eq!(assistant_blocks[1]["type"], "tool_use");
        assert_eq!(assistant_blocks[1]["id"], "call_1");
        assert_eq!(assistant_blocks[1]["input"]["path"], "a.txt");
        let tool_block = &messages[2]["content"][0];
        assert_eq!(tool_block["type"], "tool_result");
        assert_eq!(tool_block["tool_use_id"], "call_1");
        assert_eq!(payload["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn payload_uses_request_max_tokens() {
        let mut request = request(vec![message(MessageRole::User, Some("hi"))]);
        request.max_tokens = Some(64);
        assert_eq!(anthropic_payload(&request)["max_tokens"], 64);
    }

    async fn collect(events: &[&str]) -> Vec<ModelEvent> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
        let mut state = AnthropicStreamState::new("claude-sonnet-4-5".to_owned());
        for line in events {
            if state.process_line(line, &sender).await.unwrap() {
                break;
            }
        }
        drop(sender);
        let mut out = Vec::new();
        while let Some(item) = receiver.recv().await {
            out.push(item.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn stream_maps_typed_events() {
        let events = collect(&[
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":12,"output_tokens":0}}}"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"read_file"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}"#,
            r#"data: {"type":"content_block_stop","index":1}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
            r#"data: {"type":"message_stop"}"#,
        ])
        .await;
        assert_eq!(events[0], ModelEvent::Content("hello".to_owned()));
        let ModelEvent::ToolCall(call) = &events[1] else {
            panic!("expected tool call: {events:?}");
        };
        assert_eq!(call.call_id, "tu_1");
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments["path"], "a.txt");
        let ModelEvent::Usage(usage) = &events[2] else {
            panic!("expected usage: {events:?}");
        };
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 19);
        // claude-sonnet-4* prices at 3000/15000 µ$ per 1k tokens.
        assert_eq!(usage.cost_micro_usd, Some((12 * 3000 + 7 * 15000) / 1000));
        assert_eq!(
            events[3],
            ModelEvent::Finish {
                reason: Some("tool_use".to_owned())
            }
        );
    }

    #[tokio::test]
    async fn error_event_maps_to_provider_error() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let mut state = AnthropicStreamState::new("m".to_owned());
        let error = state
            .process_line(
                r#"data: {"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
                &sender,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "provider_overloaded_error");
        assert!(error.retryable);
    }

    #[test]
    fn message_serializes_image_parts_as_base64_blocks() {
        let user = anthropic_message(&Message::with_parts(
            MessageRole::User,
            "look\n[image: artifact-1]",
            vec![ModelContentPart::Image {
                media_type: "image/png".to_owned(),
                data_base64: "aGk=".to_owned(),
            }],
        ));
        assert_eq!(user["role"], "user");
        assert_eq!(
            user["content"],
            json!([
                {"type": "text", "text": "look\n[image: artifact-1]"},
                {
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/png", "data": "aGk="}
                },
            ])
        );

        // A tool result carrying view_image output parts serializes the same
        // blocks inside its tool_result content.
        let mut tool = message(MessageRole::Tool, Some("{\"image\": \"shot.png\"}"));
        tool.tool_call_id = Some("call_1".to_owned());
        tool.parts = Some(vec![ModelContentPart::Image {
            media_type: "image/png".to_owned(),
            data_base64: "aGk=".to_owned(),
        }]);
        let value = anthropic_message(&tool);
        assert_eq!(value["content"][0]["type"], "tool_result");
        assert_eq!(value["content"][0]["tool_use_id"], "call_1");
        assert_eq!(
            value["content"][0]["content"],
            json!([
                {"type": "text", "text": "{\"image\": \"shot.png\"}"},
                {
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/png", "data": "aGk="}
                },
            ])
        );
    }
}
