//! M7 provider-neutral agent loop and trusted tool runtime.
//!
//! Providers propose content and tool intents. The core owns policy, approval,
//! execution, canonical events, cancellation, budgets and history.

mod context;
mod loop_runtime;
mod provider;
mod tools;

pub use context::{
    Compaction, Message, MessageRole, ToolCall, compact_history, estimate_history_tokens,
    load_project_instructions,
};
pub use loop_runtime::{
    AgentLimits, AgentRequest, AgentRuntime, ApprovalGate, ApprovalRequest, AutoApprovalGate,
    CancelSignal, EventSink, RunOutcome, RuntimeError, StoreEventSink, SubagentRequest,
    history_from_events, mask_canonical_event,
};
pub use provider::{
    ModelDriver, ModelEvent, ModelRequest, OpenAiCompatibleDriver, ProviderError, ScriptedDriver,
    Usage,
};
pub use tools::{
    PythonFallbackTool, Tool, ToolContext, ToolDefinition, ToolError, ToolHandler, ToolRegistry,
    ToolResult, builtin_registry,
};
