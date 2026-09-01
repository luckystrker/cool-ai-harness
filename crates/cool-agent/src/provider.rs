use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use cool_security::NetworkPolicy;
use futures_util::{Stream, StreamExt as _, stream};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use url::Url;

use crate::context::{Message, MessageRole, ToolCall};
use crate::tools::ToolDefinition;

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelEvent, ProviderError>> + Send>>;
type Script = Result<Vec<ModelEvent>, ProviderError>;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_micro_usd: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelEvent {
    Content(String),
    Reasoning(String),
    ToolCall(ToolCall),
    Usage(Usage),
    Finish { reason: Option<String> },
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderError {}

#[async_trait]
pub trait ModelDriver: Send + Sync {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError>;
}

#[derive(Clone)]
pub struct ScriptedDriver {
    scripts: Arc<Mutex<VecDeque<Script>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    echo: bool,
    echo_delay: std::time::Duration,
}

impl ScriptedDriver {
    pub fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(scripts.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
            echo: false,
            echo_delay: std::time::Duration::ZERO,
        }
    }

    pub fn echo() -> Self {
        Self {
            scripts: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            echo: true,
            echo_delay: std::time::Duration::ZERO,
        }
    }

    pub fn echo_with_delay(delay: std::time::Duration) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            echo: true,
            echo_delay: delay,
        }
    }

    pub async fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().await.clone()
    }
}

#[async_trait]
impl ModelDriver for ScriptedDriver {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        self.requests.lock().await.push(request.clone());
        if self.echo {
            let content = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::User)
                .and_then(|message| message.content.clone())
                .unwrap_or_default();
            let events = VecDeque::from(vec![
                Ok(ModelEvent::Content(content)),
                Ok(ModelEvent::Finish {
                    reason: Some("stop".to_owned()),
                }),
            ]);
            let delay = self.echo_delay;
            return Ok(Box::pin(stream::unfold(
                events,
                move |mut events| async move {
                    let event = events.pop_front()?;
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    Some((event, events))
                },
            )));
        }
        let script = self.scripts.lock().await.pop_front().ok_or_else(|| {
            ProviderError::new(
                "script_exhausted",
                "scripted provider has no response",
                false,
            )
        })??;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }
}

#[derive(Clone)]
pub struct OpenAiCompatibleDriver {
    base_url: Url,
    api_key: Option<String>,
    network_policy: NetworkPolicy,
}

impl OpenAiCompatibleDriver {
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
        Ok(Self {
            base_url,
            api_key: Some(api_key.into()).filter(|value| !value.is_empty()),
            network_policy,
        })
    }
}

#[async_trait]
impl ModelDriver for OpenAiCompatibleDriver {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let url = self.base_url.join("chat/completions").map_err(|error| {
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
        let payload = openai_payload(&request);
        let mut request_builder = client.post(url).json(&payload);
        if let Some(api_key) = &self.api_key {
            request_builder = request_builder.bearer_auth(api_key);
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
                "provider_http",
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
            let mut calls: BTreeMap<u64, OpenAiToolAccumulator> = BTreeMap::new();
            let mut finish_reason = None;
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
                    match process_sse_line(&line, &sender, &mut calls, &mut finish_reason).await {
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
            for call in calls.into_values() {
                let _ = sender.send(call.finish()).await;
            }
            let _ = sender
                .send(Ok(ModelEvent::Finish {
                    reason: finish_reason,
                }))
                .await;
        });
        Ok(Box::pin(stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|item| (item, receiver))
        })))
    }
}

fn decode_sse_lines(buffer: &mut Vec<u8>, chunk: &[u8]) -> Result<Vec<String>, ProviderError> {
    buffer.extend_from_slice(chunk);
    let mut lines = Vec::new();
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer.drain(..=end).collect::<Vec<_>>();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        lines.push(
            String::from_utf8(line)
                .map_err(|error| ProviderError::new("provider_utf8", error.to_string(), false))?,
        );
    }
    Ok(lines)
}

fn openai_payload(request: &ModelRequest) -> Value {
    let messages = request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let mut value = json!({"role": role, "content": message.content});
            if !message.tool_calls.is_empty() {
                value["tool_calls"] = Value::Array(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.call_id,
                                "type": "function",
                                "function": {"name": call.name, "arguments": Value::Object(call.arguments.clone()).to_string()}
                            })
                        })
                        .collect(),
                );
            }
            if let Some(call_id) = &message.tool_call_id {
                value["tool_call_id"] = Value::String(call_id.clone());
            }
            value
        })
        .collect::<Vec<_>>();
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({"type": "function", "function": {"name": tool.name, "description": tool.description, "parameters": tool.parameters}})
        })
        .collect::<Vec<_>>();
    json!({
        "model": request.model,
        "messages": messages,
        "tools": tools,
        "stream": true,
        "stream_options": {"include_usage": true},
        "temperature": request.temperature,
        "max_tokens": request.max_tokens,
    })
}

#[derive(Default)]
struct OpenAiToolAccumulator {
    id: String,
    name: String,
    arguments: String,
}

impl OpenAiToolAccumulator {
    fn finish(self) -> Result<ModelEvent, ProviderError> {
        let arguments = if self.arguments.is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(&self.arguments).map_err(|error| {
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

async fn process_sse_line(
    line: &str,
    sender: &tokio::sync::mpsc::Sender<Result<ModelEvent, ProviderError>>,
    calls: &mut BTreeMap<u64, OpenAiToolAccumulator>,
    finish_reason: &mut Option<String>,
) -> Result<bool, ProviderError> {
    let Some(data) = line.strip_prefix("data:").map(str::trim) else {
        return Ok(false);
    };
    if data == "[DONE]" {
        for (_, call) in std::mem::take(calls) {
            sender.send(call.finish()).await.ok();
        }
        sender
            .send(Ok(ModelEvent::Finish {
                reason: finish_reason.take(),
            }))
            .await
            .ok();
        return Ok(true);
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|error| ProviderError::new("provider_json", error.to_string(), false))?;
    if let Some(usage) = value.get("usage").filter(|value| !value.is_null()) {
        sender
            .send(Ok(ModelEvent::Usage(Usage {
                prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
                completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
                total_tokens: usage["total_tokens"].as_u64().unwrap_or(0),
                cost_micro_usd: None,
            })))
            .await
            .ok();
    }
    let Some(choice) = value["choices"]
        .as_array()
        .and_then(|choices| choices.first())
    else {
        return Ok(false);
    };
    let delta = &choice["delta"];
    if let Some(content) = delta["content"].as_str().filter(|value| !value.is_empty()) {
        sender
            .send(Ok(ModelEvent::Content(content.to_owned())))
            .await
            .ok();
    }
    if let Some(reasoning) = delta["reasoning_content"]
        .as_str()
        .or_else(|| delta["reasoning"].as_str())
        .filter(|value| !value.is_empty())
    {
        sender
            .send(Ok(ModelEvent::Reasoning(reasoning.to_owned())))
            .await
            .ok();
    }
    if let Some(tool_calls) = delta["tool_calls"].as_array() {
        for call in tool_calls {
            let index = call["index"].as_u64().unwrap_or(0);
            let accumulator = calls.entry(index).or_default();
            if let Some(id) = call["id"].as_str() {
                accumulator.id.push_str(id);
            }
            if let Some(name) = call["function"]["name"].as_str() {
                accumulator.name.push_str(name);
            }
            if let Some(arguments) = call["function"]["arguments"].as_str() {
                accumulator.arguments.push_str(arguments);
            }
        }
    }
    if let Some(reason) = choice["finish_reason"].as_str() {
        for (_, call) in std::mem::take(calls) {
            sender.send(call.finish()).await.ok();
        }
        *finish_reason = Some(reason.to_owned());
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[tokio::test]
    async fn openai_sse_parser_streams_text_and_reassembles_tool_arguments() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
        let mut calls = BTreeMap::new();
        let mut finish_reason = None;
        process_sse_line(
            r#"data: {"choices":[{"delta":{"content":"hi","tool_calls":[{"index":0,"id":"call-","function":{"name":"write_","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            &sender,
            &mut calls,
            &mut finish_reason,
        )
        .await
        .unwrap();
        process_sse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"1","function":{"name":"file","arguments":"\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            &sender,
            &mut calls,
            &mut finish_reason,
        )
        .await
        .unwrap();
        process_sse_line(
            r#"data: {"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}"#,
            &sender,
            &mut calls,
            &mut finish_reason,
        )
        .await
        .unwrap();
        assert!(
            process_sse_line("data: [DONE]", &sender, &mut calls, &mut finish_reason)
                .await
                .unwrap()
        );
        assert_eq!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Content("hi".to_owned())
        );
        let ModelEvent::ToolCall(call) = receiver.recv().await.unwrap().unwrap() else {
            panic!("expected assembled tool call")
        };
        assert_eq!(call.call_id, "call-1");
        assert_eq!(call.name, "write_file");
        assert_eq!(call.arguments["path"], "a.txt");
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Usage(Usage {
                total_tokens: 5,
                ..
            })
        ));
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ModelEvent::Finish { .. }
        ));
    }

    #[test]
    fn sse_line_decoder_preserves_utf8_split_across_network_chunks() {
        let encoded = "data: {\"text\":\"привет\"}\n".as_bytes();
        let split = encoded
            .windows(2)
            .position(|bytes| bytes[0] >= 0xc0 && bytes[1] >= 0x80)
            .unwrap()
            + 1;
        let mut buffer = Vec::new();
        assert!(
            decode_sse_lines(&mut buffer, &encoded[..split])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            decode_sse_lines(&mut buffer, &encoded[split..]).unwrap(),
            ["data: {\"text\":\"привет\"}"]
        );
    }

    #[tokio::test]
    async fn local_openai_compatible_stream_works_without_authorization_header() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let count = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
            assert!(request.starts_with("post /v1/chat/completions "));
            assert!(!request.contains("authorization:"));
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"local\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let driver = OpenAiCompatibleDriver::new(
            &format!("http://{address}/v1"),
            "",
            NetworkPolicy::new([address.ip().to_string()]).loopback_only(),
        )
        .unwrap();
        let mut stream = driver
            .stream(ModelRequest {
                model: "local-model".to_owned(),
                messages: vec![Message::text(MessageRole::User, "hello")],
                tools: Vec::new(),
                temperature: 0.0,
                max_tokens: Some(8),
            })
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        server.await.unwrap();
        assert_eq!(
            events,
            vec![
                ModelEvent::Content("local".to_owned()),
                ModelEvent::Finish {
                    reason: Some("stop".to_owned())
                }
            ]
        );
    }

    #[test]
    fn provider_base_url_keeps_its_version_path() {
        let driver = OpenAiCompatibleDriver::new(
            "https://api.openai.com/v1",
            "secret",
            NetworkPolicy::new(["api.openai.com".to_owned()]),
        )
        .unwrap();
        assert_eq!(
            driver.base_url.join("chat/completions").unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }
}
