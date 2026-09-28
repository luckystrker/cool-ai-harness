//! M7 provider-neutral agent loop and trusted tool runtime.
//!
//! Providers propose content and tool intents. The core owns policy, approval,
//! execution, canonical events, cancellation, budgets and history.

mod anthropic;
mod context;
mod loop_runtime;
mod pricing;
mod provider;
mod tools;
mod web_tools;

pub use anthropic::AnthropicDriver;
pub use context::{
    COMPACTION_KEEP_LAST_GROUPS, COMPACTION_SUMMARY_PREFIX, Compaction, Message, MessageRole,
    PLANNING_SYSTEM_PROMPT, ToolCall, compact_history, default_agent_system_prompt,
    estimate_history_tokens, is_summary_message, load_project_instructions, load_task_progress,
    planning_system_prompt, summary_drop_candidates,
};
pub use loop_runtime::{
    AgentLimits, AgentRequest, AgentRuntime, ApprovalGate, ApprovalRequest, AutoApprovalGate,
    CancelSignal, EventSink, RunOutcome, RuntimeError, StoreEventSink, SubagentRequest,
    history_from_event_rows, history_from_events, mask_canonical_event,
};
pub use pricing::{estimate_cost_micro_usd, has_pricing, model_pricing};
pub use provider::{
    ModelDriver, ModelEvent, ModelRequest, ModelStream, OpenAiCompatibleDriver, ProviderError,
    ScriptedDriver, Usage,
};
pub use tools::{
    PythonFallbackTool, Tool, ToolCatalogEntry, ToolContext, ToolDefinition, ToolError,
    ToolHandler, ToolRegistry, ToolResult, builtin_registry, capability_name,
};
pub use web_tools::{WebToolsConfig, web_tool_registry};
