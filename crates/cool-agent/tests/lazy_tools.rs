//! P1.10 lazy tool loading: deferred tools hide from `definitions()` until
//! `activate_tools` names them, `search_tools` discovers them in the catalog,
//! and activation never bypasses `policy.evaluate`.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, EventSink,
    GateOutcome, ModelEvent, ScriptedDriver, Tool, ToolContext, ToolDefinition, ToolError,
    ToolHandler, ToolResult, builtin_registry,
};
use cool_agent::{ApprovalGate, ApprovalRequest};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, CanonicalEvent, EventEnvelope, V1Version,
};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use serde_json::{Value, json};
use tempfile::tempdir;

struct RecordingSink {
    events: Mutex<Vec<EventEnvelope>>,
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, cool_agent::RuntimeError> {
        let mut events = self.events.lock().unwrap();
        let envelope = EventEnvelope {
            event_id: format!("event-{}", events.len() + 1),
            schema_version: V1Version::VALUE,
            session_id: "session".to_owned(),
            run_id: "run".to_owned(),
            item_id: None,
            seq: events.len() as u64 + 1,
            occurred_at: "test".to_owned(),
            actor: ActorRef {
                id: "cool-agent".to_owned(),
                kind: ActorKind::System,
            },
            source: "test".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: BTreeMap::new(),
        };
        events.push(envelope.clone());
        Ok(envelope)
    }
}

struct RecordingGate {
    requests: Mutex<Vec<ApprovalRequest>>,
}

#[async_trait]
impl ApprovalGate for RecordingGate {
    async fn request(
        &self,
        request: ApprovalRequest,
        _sink: &dyn EventSink,
        _cancel: &mut CancelSignal,
    ) -> Result<GateOutcome, cool_agent::RuntimeError> {
        self.requests.lock().unwrap().push(request);
        Ok(GateOutcome::decided(ApprovalOutcome::Approved))
    }
}

struct EchoTool;

#[async_trait]
impl ToolHandler for EchoTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::ok(json!("echoed")))
    }
}

fn deferred_tool(name: &str, description: &str, decision: Decision) -> Tool {
    Tool::new(
        ToolDefinition {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({"type": "object"}),
        },
        [],
        decision,
        EchoTool,
    )
    .deferred()
}

fn request(workspace: &std::path::Path) -> AgentRequest {
    AgentRequest {
        model: "scripted".to_owned(),
        history: Vec::new(),
        user_input: "hi".to_owned(),
        system_prompt: Some("be precise".to_owned()),
        mode: None,
        temperature: 0.0,
        max_tokens: None,
        limits: AgentLimits::default(),
        tool_names: None,
        tool_context: ToolContext::new(
            Workspace::new(workspace).unwrap(),
            CapabilityPolicy::new(Some(Decision::Allow)),
        ),
    }
}

fn tool_call(call_id: &str, name: &str, arguments: Value) -> ModelEvent {
    ModelEvent::ToolCall(cool_agent::ToolCall {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
    })
}

fn tool_names(request: &cool_agent::ModelRequest) -> Vec<&str> {
    request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect()
}

#[tokio::test]
async fn deferred_tool_is_hidden_until_activate_tools_names_it() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    registry
        .register(deferred_tool("demo_hidden", "hidden demo", Decision::Allow))
        .unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            tool_call(
                "call-1",
                "activate_tools",
                json!({"names": ["demo_hidden", "no_such_tool"]}),
            ),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]));
    let runtime = AgentRuntime::new(provider.clone(), registry);
    let sink = RecordingSink {
        events: Mutex::new(Vec::new()),
    };
    let (_, cancel) = CancelSignal::channel();
    runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();

    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2);
    let first = tool_names(&requests[0]);
    assert!(
        !first.contains(&"demo_hidden"),
        "deferred tool hidden at start"
    );
    assert!(first.contains(&"search_tools") && first.contains(&"activate_tools"));
    // The hidden-tools hint lands on the leading system message.
    let system = requests[0]
        .messages
        .iter()
        .find(|message| message.role == cool_agent::MessageRole::System)
        .and_then(|message| message.content.clone())
        .unwrap_or_default();
    assert!(
        system.contains("search_tools") && system.contains("activate_tools"),
        "hidden-tools hint in the system prompt: {system}"
    );
    // The tool batch ran activate_tools before turn 2: activated + unknown.
    let tool_result = requests[1]
        .messages
        .iter()
        .find(|message| message.role == cool_agent::MessageRole::Tool)
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .unwrap_or_default();
    assert!(tool_result.contains("demo_hidden") && tool_result.contains("no_such_tool"));
    let second = tool_names(&requests[1]);
    assert!(
        second.contains(&"demo_hidden"),
        "activated tool ships next turn"
    );
}

#[tokio::test]
async fn search_tools_finds_deferred_tools_by_description() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    registry
        .register(deferred_tool(
            "mcp_metrics_collect",
            "Collects server-side metrics from the observability stack",
            Decision::Allow,
        ))
        .unwrap();
    let search = registry.get("search_tools").expect("meta tool");
    let context = ToolContext::new(
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    );
    let result = search
        .execute(&context, json!({"query": "server-side metrics"}))
        .await
        .expect("search executes");
    let text = serde_json::to_string(&result.output).unwrap_or_default();
    assert!(
        text.contains("mcp_metrics_collect"),
        "deferred tool found by description: {text}"
    );
    let tools = result
        .output
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let entry = tools
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some("mcp_metrics_collect"))
        .expect("entry");
    assert_eq!(entry.get("deferred").and_then(Value::as_bool), Some(true));
}

/// Review follow-up: `extend` builds a fresh backing map — the meta-tools
/// must be rebound to it or deferred tools added by the host (MCP, executor
/// tools) can never be discovered or activated.
#[tokio::test]
async fn extended_registry_keeps_deferred_tools_discoverable() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry()
        .extend([deferred_tool(
            "mcp_weather_lookup",
            "Weather lookup from an MCP server added after startup",
            Decision::Allow,
        )])
        .expect("extend");
    let context = ToolContext::new(
        Workspace::new(directory.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    );
    let search = registry.get("search_tools").expect("meta tool");
    let result = search
        .execute(&context, json!({"query": "weather"}))
        .await
        .expect("search executes");
    let text = serde_json::to_string(&result.output).unwrap_or_default();
    assert!(
        text.contains("mcp_weather_lookup"),
        "extension tool discoverable after extend: {text}"
    );
    let activate = registry.get("activate_tools").expect("meta tool");
    let result = activate
        .execute(&context, json!({"names": ["mcp_weather_lookup"]}))
        .await
        .expect("activate executes");
    assert!(
        result
            .output
            .get("activated")
            .and_then(Value::as_array)
            .is_some_and(|names| names
                .iter()
                .any(|name| name.as_str() == Some("mcp_weather_lookup"))),
        "extension tool activatable after extend: {:?}",
        result.output
    );
}

#[tokio::test]
async fn activated_tool_still_requires_approval() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    registry
        .register(deferred_tool(
            "guarded",
            "an approval-gated tool",
            Decision::Ask,
        ))
        .unwrap();
    let provider = Arc::new(ScriptedDriver::new([
        Ok(vec![
            tool_call("call-1", "activate_tools", json!({"names": ["guarded"]})),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
        Ok(vec![
            tool_call("call-2", "guarded", json!({})),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]));
    let runtime = AgentRuntime::new(provider.clone(), registry);
    let sink = RecordingSink {
        events: Mutex::new(Vec::new()),
    };
    let gate = RecordingGate {
        requests: Mutex::new(Vec::new()),
    };
    let (_, cancel) = CancelSignal::channel();
    runtime
        .run(request(directory.path()), &sink, &gate, cancel)
        .await
        .unwrap();
    // activate_tools itself needs no approval; the activated Ask tool still
    // went through the gate once — activation is not permission.
    let requests = gate.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].call.name, "guarded");
}

#[test]
fn mcp_tools_defer_only_above_the_eager_limit() {
    let registry = builtin_registry();
    registry
        .register(Tool::new(
            ToolDefinition {
                name: "mcp_demo_lookup".to_owned(),
                description: "an mcp tool".to_owned(),
                parameters: json!({"type": "object"}),
            },
            [],
            Decision::Allow,
            EchoTool,
        ))
        .unwrap();
    // Below the limit the mcp tool is eager.
    assert!(!registry.is_tool_deferred("mcp_demo_lookup"));
    // Push the catalog past COOL_EAGER_TOOL_LIMIT — mcp_* defers now.
    for index in 0..10 {
        registry
            .register(Tool::new(
                ToolDefinition {
                    name: format!("extra_tool_{index}"),
                    description: "filler".to_owned(),
                    parameters: json!({"type": "object"}),
                },
                [],
                Decision::Allow,
                EchoTool,
            ))
            .unwrap();
    }
    assert!(registry.is_tool_deferred("mcp_demo_lookup"));
    let visible: Vec<String> = registry
        .visible_definitions(&Default::default())
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(!visible.contains(&"mcp_demo_lookup".to_owned()));
    assert!(visible.contains(&"read_file".to_owned()));
    assert!(visible.contains(&"extra_tool_0".to_owned()));
    assert!(registry.has_deferred_tools());
    let mut active = std::collections::BTreeSet::new();
    active.insert("mcp_demo_lookup".to_owned());
    let visible: Vec<String> = registry
        .visible_definitions(&active)
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert!(visible.contains(&"mcp_demo_lookup".to_owned()));
}

/// Deferred is not just an advertising filter: a tool the model guesses by
/// name fails before policy evaluation until `activate_tools` names it.
#[tokio::test]
async fn unactivated_deferred_tool_call_fails_tool_not_active() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    registry
        .register(deferred_tool("demo_hidden", "hidden demo", Decision::Allow))
        .unwrap();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        tool_call("call-1", "demo_hidden", json!({})),
        ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        },
    ])]));
    let runtime = AgentRuntime::new(provider.clone(), registry);
    let sink = RecordingSink {
        events: Mutex::new(Vec::new()),
    };
    let (_, cancel) = CancelSignal::channel();
    runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();

    let tool_result = provider
        .requests()
        .await
        .iter()
        .flat_map(|request| request.messages.iter())
        .find(|message| message.role == cool_agent::MessageRole::Tool)
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .unwrap_or_default();
    assert!(
        tool_result.contains("activate_tools"),
        "deferred call rejected with an activation hint: {tool_result}"
    );
}

/// A profile's `tool_names` allowlist gates execution, not just visibility:
/// a registered but unlisted tool cannot be invoked by name (P2.15's
/// read-only guarantee).
#[tokio::test]
async fn tool_names_allowlist_blocks_unlisted_tool_calls() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let provider = Arc::new(ScriptedDriver::new([Ok(vec![
        tool_call("call-1", "list_files", json!({})),
        ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        },
    ])]));
    let runtime = AgentRuntime::new(provider.clone(), registry);
    let sink = RecordingSink {
        events: Mutex::new(Vec::new()),
    };
    let mut request = request(directory.path());
    request.tool_names = Some(["read_file".to_owned()].into_iter().collect());
    let (_, cancel) = CancelSignal::channel();
    runtime
        .run(
            request,
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();

    let tool_result = provider
        .requests()
        .await
        .iter()
        .flat_map(|request| request.messages.iter())
        .find(|message| message.role == cool_agent::MessageRole::Tool)
        .map(|message| serde_json::to_string(message).unwrap_or_default())
        .unwrap_or_default();
    assert!(
        tool_result.contains("allowlist"),
        "unlisted tool call rejected: {tool_result}"
    );
}
