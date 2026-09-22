// Record -> view-type mappers for the canonical App Protocol boundary.
//
// The generated protocol records use camelCase and JsonValue attributes; the
// hand-written `types.ts` mirrors the Python Pydantic schemas (snake_case).
// Keeping the conversion here lets `src/api/*` present the same shapes to the
// pages whether a call is served by Rust or by the Python fallback.
import type * as protocol from "./generated/cool_protocol"
import type {
  AgentProfile,
  AnalyticsSummary,
  ApprovalAudit,
  Artifact,
  ArtifactDetail,
  ArtifactKind,
  BudgetStatusResponse,
  BudgetWindowSpend,
  CallHistoryResponse,
  CallHistoryRow,
  CapabilityPolicy,
  Conversation,
  Entity,
  Episode,
  IterationDetail,
  LatencyPoint,
  MacroStep,
  MacroTool,
  MCPServer,
  MCPToolInfo,
  MemoryActivityPoint,
  MemoryExplain,
  MemoryItem,
  MemoryStats,
  Message,
  ModelInfo,
  ModelSpend,
  ParseCronResponse,
  Plan,
  PlanStatus,
  PlanStep,
  PlanStepStatus,
  PlanTemplate,
  Provider,
  ResearchCitation,
  ResearchRun,
  ResearchRunDetail,
  ResearchSource,
  ResearchStatus,
  ResearchUsage,
  ReplayResponse,
  RssEntry,
  RssSubscription,
  RunComparison,
  RunDetail,
  RunEventOut,
  RunOut,
  RunTimeline,
  ScheduledTask,
  SchedulerStatus,
  SpendRow,
  SpendTimeSeriesPoint,
  SubagentRole,
  SubagentRun,
  SubagentRunDetail,
  TaskInbox,
  TaskRun,
  TaskRunDetail,
  TaskTemplate,
  ToolCall,
  ToolCatalogItem,
  ToolPermissions,
  ToolResultPayload,
  TopTool,
  WebhookEndpoint,
  WebhookEvent,
  WikiArticle,
} from "./types"

function asObject(value: unknown): Record<string, unknown> | null {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return null
}

function asArray(value: unknown): protocol.JsonValue[] {
  return Array.isArray(value) ? (value as protocol.JsonValue[]) : []
}

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null
}

function asStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return value.filter((item): item is string => typeof item === "string")
}

function asOptionalStringArray(value: unknown): string[] | null {
  if (value === null || value === undefined) return null
  return asStringArray(value)
}

function parsedJson(value: string | null): protocol.JsonValue | null {
  if (value === null) return null
  try {
    return JSON.parse(value) as protocol.JsonValue
  } catch {
    return value
  }
}

/** `ConversationRecord` -> `Conversation` (breakpoints live in metadata). */
export function toMcpTool(record: protocol.McpToolRecord): MCPToolInfo {
  return {
    name: record.name,
    qualified_name: record.qualifiedName,
    description: record.description,
    server_name: record.serverName,
    input_schema: (record.inputSchema ?? {}) as Record<string, unknown>,
  }
}

export function toMcpServer(record: protocol.McpServerAdminRecord): MCPServer {
  return {
    name: record.name,
    transport: record.transport as MCPServer["transport"],
    status: record.status as MCPServer["status"],
    enabled: record.enabled,
    description: record.description,
    command: record.command,
    args: record.args,
    url: record.url,
    capabilities: record.capabilities,
    timeout_s: record.timeoutS,
    error: record.error ?? null,
    tools: (record.tools ?? []).map(toMcpTool),
    server_info: (record.serverInfo ?? {}) as Record<string, unknown>,
  }
}

export function toConversation(record: protocol.ConversationRecord): Conversation {
  const metadata = asObject(record.metadata)
  return {
    id: record.id,
    user_id: record.userId,
    title: record.title,
    model: record.model,
    working_directory: record.workingDirectory,
    permissions: (record.permissions as unknown as ToolPermissions | null) ?? null,
    capability_policy: (record.capabilityPolicy as unknown as CapabilityPolicy | null) ?? null,
    breakpoints: (metadata?.breakpoints as Conversation["breakpoints"] | undefined) ?? null,
    profile_id: record.profileId,
    tags: asStringArray(record.tags),
    folder: record.folder,
    is_pinned: record.isPinned,
    is_archived: record.isArchived,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

/** `ConversationCreate` -> canonical create params. */
export function toConversationCreate(
  body: {
    title?: string
    model?: string
    working_directory?: string
    permissions?: ToolPermissions
    capability_policy?: CapabilityPolicy
    breakpoints?: Conversation["breakpoints"]
    profile_id?: number
  },
  idempotencyKey: string
): protocol.ConversationCreateParams {
  return {
    idempotencyKey,
    title: body.title ?? null,
    provider: null,
    model: body.model ?? null,
    workingDirectory: body.working_directory ?? null,
    permissions: (body.permissions as unknown as protocol.JsonValue | undefined) ?? null,
    capabilityPolicy: (body.capability_policy as unknown as protocol.JsonValue | undefined) ?? null,
    profileId: body.profile_id ?? null,
    tags: null,
    folder: null,
    metadata: body.breakpoints ? { breakpoints: body.breakpoints as unknown as protocol.JsonValue } : null,
  }
}

/** `ConversationUpdate` -> canonical update params. */
export function toConversationUpdate(
  id: number,
  body: {
    title?: string
    model?: string
    working_directory?: string
    permissions?: ToolPermissions
    capability_policy?: CapabilityPolicy
    breakpoints?: Conversation["breakpoints"]
    profile_id?: number
    tags?: string[]
    folder?: string
    is_pinned?: boolean
    is_archived?: boolean
  },
  idempotencyKey: string
): protocol.ConversationUpdateParams {
  return {
    idempotencyKey,
    id,
    title: body.title ?? null,
    provider: null,
    model: body.model ?? null,
    workingDirectory: body.working_directory ?? null,
    permissions: (body.permissions as unknown as protocol.JsonValue | undefined) ?? null,
    capabilityPolicy: (body.capability_policy as unknown as protocol.JsonValue | undefined) ?? null,
    profileId: body.profile_id ?? null,
    tags: (body.tags as unknown as protocol.JsonValue | undefined) ?? null,
    folder: body.folder ?? null,
    isPinned: body.is_pinned ?? null,
    isArchived: body.is_archived ?? null,
    metadata: body.breakpoints ? { breakpoints: body.breakpoints as unknown as protocol.JsonValue } : null,
  }
}

function toToolResult(item: protocol.HistoryItem): Message["tool_result"] {
  if (item.role !== "tool") return null
  const parsed = parsedJson(item.content)
  let result: ToolResultPayload
  if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const object = parsed as Record<string, unknown>
    if (typeof object.errorCode === "string") {
      const message = typeof object.error === "string" ? object.error : object.errorCode
      result = { output: message, is_error: true, error: message }
    } else {
      result = { output: JSON.stringify(object), is_error: false, metadata: object }
    }
  } else {
    const output = typeof parsed === "string" ? parsed : parsed === null ? "" : JSON.stringify(parsed)
    result = { output, is_error: false }
  }
  return { tool_call_id: item.toolCallId, name: item.name, result }
}

function toUsage(item: protocol.HistoryItem): Record<string, unknown> | null {
  if (!item.usage) return null
  return {
    prompt_tokens: item.usage.promptTokens,
    completion_tokens: item.usage.completionTokens,
    total_tokens: item.usage.totalTokens,
    cost_usd: item.usage.costUsd,
  }
}

/** One canonical `HistoryItem` -> the stored-message shape the chat renders. */
export function toMessage(item: protocol.HistoryItem, conversationId: number): Message {
  const toolCalls: ToolCall[] | null = item.toolCalls.length
    ? item.toolCalls.map((call) => ({
        id: call.callId,
        name: call.name,
        arguments: call.arguments as Record<string, unknown>,
      }))
    : null
  return {
    id: item.cursor,
    conversation_id: conversationId,
    role: item.role as Message["role"],
    content: item.content,
    tool_calls: toolCalls,
    tool_result: toToolResult(item),
    usage: toUsage(item),
    thinking: item.reasoning,
    model: item.model,
    created_at: item.occurredAt,
  }
}

/** One legacy `MessageRecord` -> `Message` (subagent/task detail transcripts). */
export function toLegacyMessage(record: protocol.MessageRecord): Message {
  const calls: ToolCall[] | null = Array.isArray(record.toolCalls)
    ? (record.toolCalls as unknown as ToolCall[])
    : null
  return {
    id: record.id,
    conversation_id: record.conversationId,
    role: record.role as Message["role"],
    content: record.content,
    tool_calls: calls,
    tool_result: (record.toolResult as unknown as Message["tool_result"]) ?? null,
    usage: asObject(record.usage),
    thinking: record.thinking,
    model: record.model,
    duration_ms: record.durationMs,
    created_at: record.createdAt,
  }
}

/** `SessionRunSummary` -> legacy `RunOut` (the recorder only reads status). */
export function toRun(
  summary: protocol.SessionRunSummary,
  conversationId: number,
  /** Position in the newest-first list; keeps the legacy id sort stable. */
  index = 0
): RunOut {
  return {
    id: -index,
    conversation_id: conversationId,
    status: summary.status,
    model: null,
    iterations: 0,
    usage: null,
    finish_reason: summary.finishReason,
    error: null,
    started_at: summary.updatedAt,
    finished_at: null,
    created_at: summary.updatedAt,
    updated_at: summary.updatedAt,
  }
}

/** `CompactResult` -> the compact response shape pages expect. */
export function toCompactResponse(result: protocol.CompactResult) {
  return {
    status: result.status,
    reason: result.reason ?? undefined,
    message_count: result.messageCount ?? undefined,
    messages_compacted: result.messagesCompacted ?? undefined,
    messages_kept: result.messagesKept ?? undefined,
    summary_length: result.summaryLength ?? undefined,
  }
}

/** `ApprovalAuditRecord` -> `ApprovalAudit` (snake_case). */
export function toApprovalAudit(record: protocol.ApprovalAuditRecord): ApprovalAudit {
  return {
    id: record.id,
    conversation_id: record.conversationId,
    run_id: record.runId,
    call_id: record.callId,
    tool_name: record.toolName,
    arguments: asObject(record.arguments),
    approved: record.approved,
    decision_source: record.decisionSource,
    decided_by: record.decidedBy,
    reason: record.reason,
    is_breakpoint: record.isBreakpoint,
    breakpoint_type: record.breakpointType,
    duration_ms: record.durationMs,
    created_at: record.createdAt,
  }
}

/** `AgentRunRecord` -> `RunOut` (the legacy runs table the inspector lists). */
export function toAgentRun(record: protocol.AgentRunRecord): RunOut {
  return {
    id: record.id,
    conversation_id: record.conversationId,
    status: record.status,
    model: record.model,
    iterations: record.iterations,
    usage: asObject(record.usage),
    finish_reason: record.finishReason,
    error: record.error,
    started_at: record.startedAt,
    finished_at: record.finishedAt,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

// --- analytics ---

export function toAnalyticsSummary(record: protocol.AnalyticsSummaryRecord): AnalyticsSummary {
  return {
    total_spend_usd: record.totalSpendUsd,
    total_llm_calls: record.totalLlmCalls,
    total_tokens: record.totalTokens,
    total_tool_calls: record.totalToolCalls,
    tool_error_count: record.toolErrorCount,
    tool_success_rate: record.toolSuccessRate,
    days: record.days,
  }
}

export function toSpendTimeSeriesPoint(record: protocol.SpendBucketRecord): SpendTimeSeriesPoint {
  return {
    period: record.period,
    cost_usd: record.costUsd,
    total_tokens: record.totalTokens,
    calls: record.calls,
  }
}

export function toModelSpend(record: protocol.ModelSpendRecord): ModelSpend {
  return {
    model: record.model,
    cost_usd: record.costUsd,
    total_tokens: record.totalTokens,
    calls: record.calls,
  }
}

export function toTopTool(record: protocol.ToolUsageRecord): TopTool {
  return {
    name: record.name,
    calls: record.calls,
    avg_duration_ms: record.avgDurationMs,
    success_rate: record.successRate,
    error_count: record.errorCount,
  }
}

export function toLatencyPoint(record: protocol.LatencyBucketRecord): LatencyPoint {
  return {
    period: record.period,
    avg_ms: record.avgMs,
    min_ms: record.minMs,
    max_ms: record.maxMs,
    calls: record.calls,
  }
}

export function toCallHistoryRow(record: protocol.CallHistoryRowRecord): CallHistoryRow {
  return {
    id: record.id,
    ts: record.ts,
    model: record.model,
    provider_name: record.providerName,
    prompt_tokens: record.promptTokens,
    completion_tokens: record.completionTokens,
    total_tokens: record.totalTokens,
    cost_usd: record.costUsd,
    run_id: record.runId,
    conversation_id: record.conversationId,
  }
}

export function toCallHistoryResponse(record: protocol.CallHistoryResult): CallHistoryResponse {
  return { rows: record.rows.map(toCallHistoryRow), total: record.total }
}

export function toMemoryActivityPoint(record: protocol.MemoryActivityBucketRecord): MemoryActivityPoint {
  return { period: record.period, created: record.created, by_type: record.byType }
}

// --- budgets ---

function toBudgetWindow(record: protocol.BudgetWindowSpendRecord): BudgetWindowSpend {
  return { spend_usd: record.spendUsd, limit_usd: record.limitUsd, pct: record.pct }
}

export function toBudgetStatus(record: protocol.BudgetStatusRecord): BudgetStatusResponse {
  return {
    status: record.status as BudgetStatusResponse["status"],
    overridden: record.overridden,
    daily: toBudgetWindow(record.daily),
    weekly: toBudgetWindow(record.weekly),
    monthly: toBudgetWindow(record.monthly),
    daily_limit_usd: record.dailyLimitUsd,
    weekly_limit_usd: record.weeklyLimitUsd,
    monthly_limit_usd: record.monthlyLimitUsd,
    alert_threshold_pct: record.alertThresholdPct,
    block_on_exceed: record.blockOnExceed,
    override_until: record.overrideUntil,
  }
}

export function toSpendRow(record: protocol.SpendEntryRecord): SpendRow {
  return {
    id: record.id,
    run_id: record.runId,
    conversation_id: record.conversationId,
    provider_name: record.providerName,
    model: record.model,
    prompt_tokens: record.promptTokens,
    completion_tokens: record.completionTokens,
    total_tokens: record.totalTokens,
    cost_usd: record.costUsd,
    ts: record.ts,
  }
}

// --- profiles ---

export function toProfile(record: protocol.ProfileRecord): AgentProfile {
  return {
    id: record.id,
    name: record.name,
    slug: record.slug,
    description: record.description,
    system_prompt: record.systemPrompt,
    model: record.model,
    tool_names: asOptionalStringArray(record.toolNames),
    skill_names: asOptionalStringArray(record.skillNames),
    settings: asObject(record.settings),
    avatar_color: record.avatarColor,
    is_builtin: record.isBuiltin,
    is_active: record.isActive,
    is_shared: record.isShared,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

// --- providers ---

export function toProvider(record: protocol.ProviderRecord): Provider {
  return {
    id: record.id,
    name: record.name,
    label: record.label,
    base_url: record.baseUrl,
    default_model: record.defaultModel,
    is_active: record.isActive,
    is_subscription: record.isSubscription,
    is_fallback: record.isFallback,
    is_default: record.isDefault,
    chat_models: asStringArray(record.chatModels),
    api_key_hint: record.apiKeyHint ?? null,
  }
}

export function toModelInfo(record: protocol.ModelInfoRecord): ModelInfo {
  return {
    id: record.id,
    context_window: record.contextWindow,
    prompt_price: record.promptPrice,
    completion_price: record.completionPrice,
  }
}

// --- artifacts ---

export function toArtifact(record: protocol.ArtifactRecord): Artifact {
  return {
    id: record.id,
    conversation_id: record.conversationId,
    run_id: record.runId,
    tool_call_id: record.toolCallId,
    filename: record.filename,
    media_type: record.mediaType,
    kind: record.kind as ArtifactKind,
    size_bytes: record.sizeBytes,
    sha256: record.sha256,
    version: record.version,
    parent_id: record.parentId,
    metadata_: asObject(record.metadata),
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toArtifactDetail(record: protocol.ArtifactDetailRecord): ArtifactDetail {
  return {
    ...toArtifact(record.artifact),
    extracted_text: record.extractedText,
    versions: record.versions.map(toArtifact),
  }
}

// --- constructor ---

function toMacroSteps(value: unknown): MacroStep[] {
  return asArray(value).flatMap((item) => {
    const object = asObject(item)
    if (!object) return []
    return [
      {
        id: String(object.id ?? ""),
        tool_name: String(object.tool_name ?? object.toolName ?? ""),
        arguments: asObject(object.arguments) ?? {},
      },
    ]
  })
}

export function toMacroTool(record: protocol.MacroToolRecord): MacroTool {
  return {
    id: record.id,
    name: record.name,
    description: record.description,
    input_schema: asObject(record.inputSchema) ?? {},
    steps: toMacroSteps(record.steps),
    is_active: record.isActive,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

// --- memory ---

export function toMemoryItem(record: protocol.MemoryRecord): MemoryItem {
  return {
    id: record.id,
    user_id: record.userId,
    scope: record.scope as MemoryItem["scope"],
    agent_id: record.agentId,
    conversation_id: record.conversationId,
    memory_type: record.memoryType as MemoryItem["memory_type"],
    content: record.content,
    structured: asObject(record.structured),
    tags: asOptionalStringArray(record.tags),
    importance: record.importance,
    confidence: record.confidence,
    source: record.source,
    status: record.status as MemoryItem["status"],
    pinned: record.pinned,
    access_count: record.accessCount,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toMemoryExplain(record: protocol.MemoryExplainRecord): MemoryExplain {
  return {
    memory_id: record.memoryId,
    source: record.source,
    scope: record.scope as MemoryExplain["scope"],
    status: record.status as MemoryExplain["status"],
    pinned: record.pinned,
    confidence: record.confidence,
    importance: record.importance,
    memory_type: record.memoryType as MemoryExplain["memory_type"],
    conversation_id: record.conversationId,
    agent_id: record.agentId,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
    last_accessed_at: record.lastAccessedAt,
    access_count: record.accessCount,
    score: {
      importance: record.score.importance,
      recency: record.score.recency,
      confidence: record.score.confidence,
      type_priority: record.score.typePriority,
      age_days: record.score.ageDays,
      total: record.score.total,
    },
  }
}

export function toMemoryStats(record: protocol.MemoryStatsRecord): MemoryStats {
  return {
    total_active: record.totalActive,
    by_type: record.byType,
    by_scope: record.byScope,
    total_episodes: record.totalEpisodes,
    total_archived: record.totalArchived,
    total_pending: record.totalPending,
    total_entities: record.totalEntities,
  }
}

export function toEpisode(record: protocol.EpisodeRecord): Episode {
  return {
    id: record.id,
    user_id: record.userId,
    agent_id: record.agentId,
    conversation_id: record.conversationId,
    title: record.title,
    summary: record.summary,
    outcome: record.outcome,
    importance: record.importance,
    tags: asOptionalStringArray(record.tags),
    created_at: record.createdAt,
  }
}

export function toEntity(record: protocol.EntityRecord): Entity {
  return {
    id: record.id,
    user_id: record.userId,
    name: record.name,
    entity_type: record.entityType,
    aliases: asOptionalStringArray(record.aliases),
    attributes: asObject(record.attributes),
    description: record.description,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

// --- plans ---

function toPlanSteps(value: unknown): PlanStep[] {
  return asArray(value).flatMap((item, index) => {
    const object = asObject(item)
    if (!object) return []
    const dependsOn = asArray(object.depends_on).filter(
      (dep): dep is number => typeof dep === "number"
    )
    const tools = asArray(object.tools).filter((tool): tool is string => typeof tool === "string")
    return [
      {
        position: typeof object.position === "number" ? object.position : index,
        title: String(object.title ?? ""),
        description: asString(object.description),
        status: (asString(object.status) as PlanStepStatus | null) ?? "pending",
        depends_on: dependsOn.length ? dependsOn : null,
        tools: tools.length ? tools : null,
        delegate_role: asString(object.delegate_role),
        result_summary: asString(object.result_summary),
      },
    ]
  })
}

export function toPlan(record: protocol.PlanRecord): Plan {
  return {
    id: record.id,
    conversation_id: record.conversationId,
    run_id: record.runId,
    title: record.title,
    status: record.status as PlanStatus,
    steps: toPlanSteps(record.steps),
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toPlanTemplate(record: protocol.PlanTemplateRecord): PlanTemplate {
  return {
    id: record.id,
    name: record.name,
    description: record.description,
    steps: toPlanSteps(record.steps),
    is_builtin: record.isBuiltin,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

// --- research ---

function toResearchUsage(value: unknown): ResearchUsage | null {
  const object = asObject(value)
  if (!object) return null
  return {
    prompt_tokens: object.prompt_tokens as number | undefined,
    completion_tokens: object.completion_tokens as number | undefined,
    total_tokens: object.total_tokens as number | undefined,
    cost_usd: object.cost_usd as number | undefined,
  }
}

export function toResearchRun(record: protocol.ResearchRunRecord): ResearchRun {
  return {
    id: record.id,
    topic: record.topic,
    depth: record.depth,
    model: record.model,
    status: record.status as ResearchStatus,
    input_hash: record.inputHash,
    report_artifact_id: record.reportArtifactId,
    sources_count: asArray(record.sources).length,
    citations_count: asArray(record.citations).length,
    usage: toResearchUsage(record.usage),
    error: record.error,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
    finished_at: record.finishedAt,
  }
}

export function toResearchRunDetail(record: protocol.ResearchRunDetailRecord): ResearchRunDetail {
  return {
    ...toResearchRun(record.run),
    conversation_id: record.conversationId,
    parent_task_run_id: record.parentTaskRunId,
    sub_questions: record.subQuestions,
    sources: asArray(record.sources) as unknown as ResearchSource[],
    citations: asArray(record.citations) as unknown as ResearchCitation[],
    report_markdown: record.reportMarkdown,
    // The canonical detail has no browser-activity projection (M11 gap).
    browser_activity: [],
  }
}

// --- RSS ---

export function toRssSubscription(record: protocol.RssSubscriptionRecord): RssSubscription {
  return {
    id: record.id,
    user_id: record.userId,
    url: record.url,
    title: record.title,
    site_url: record.siteUrl,
    category: record.category,
    fetch_interval_minutes: record.fetchIntervalMinutes,
    enabled: record.enabled,
    last_fetched_at: record.lastFetchedAt,
    last_error: record.lastError,
    entry_count: record.entryCount,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toRssEntry(record: protocol.RssEntryRecord): RssEntry {
  return {
    id: record.id,
    subscription_id: record.subscriptionId,
    guid: record.guid,
    title: record.title,
    link: record.link,
    author: record.author,
    summary: record.summary,
    published_at: record.publishedAt,
    content_hash: record.contentHash,
    is_read: record.isRead,
    fetched_at: record.fetchedAt,
  }
}

// --- subagents ---

export function toSubagentRole(record: protocol.SubagentRoleRecord): SubagentRole {
  return {
    id: record.id,
    name: record.name,
    description: record.description,
    system_prompt: record.systemPrompt,
    model: record.model,
    tool_names: asOptionalStringArray(record.toolNames),
    capability_policy: asObject(record.capabilityPolicy) as Record<string, string> | null,
    max_iterations: record.maxIterations,
    max_cost_usd: record.maxCostUsd,
    is_builtin: record.isBuiltin,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toSubagentRun(record: protocol.SubagentRunRecord): SubagentRun {
  return {
    id: record.id,
    role_id: record.roleId,
    parent_conversation_id: record.parentConversationId,
    parent_run_id: record.parentRunId,
    conversation_id: record.conversationId,
    run_id: record.runId,
    name: record.name,
    prompt: record.prompt,
    status: record.status as SubagentRun["status"],
    result_summary: record.resultSummary,
    usage: asObject(record.usage),
    error: record.error,
    started_at: record.startedAt,
    finished_at: record.finishedAt,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toSubagentRunDetail(record: protocol.SubagentRunDetailRecord): SubagentRunDetail {
  return {
    ...toSubagentRun(record.run),
    messages: record.messages.map(toLegacyMessage),
  }
}

// --- tasks ---

export function toScheduledTask(record: protocol.TaskRecord): ScheduledTask {
  return {
    id: record.id,
    user_id: record.userId,
    name: record.name,
    description: record.description,
    trigger_type: record.triggerType as ScheduledTask["trigger_type"],
    cron_expression: record.cronExpression,
    interval_seconds: record.intervalSeconds,
    run_at: record.runAt,
    timezone: record.timezone,
    quiet_hours_start: record.quietHoursStart,
    quiet_hours_end: record.quietHoursEnd,
    misfire_policy: record.misfirePolicy as ScheduledTask["misfire_policy"],
    prompt: record.prompt,
    workflow_type: record.workflowType,
    profile_id: record.profileId,
    model: record.model,
    tools_whitelist: asOptionalStringArray(record.toolsWhitelist),
    capability_policy: asObject(record.capabilityPolicy) as Record<string, string> | null,
    working_directory: record.workingDirectory,
    approval_policy: record.approvalPolicy as ScheduledTask["approval_policy"],
    delivery_channels: (record.deliveryChannels as unknown as ScheduledTask["delivery_channels"]) ?? null,
    delivery_config: asObject(record.deliveryConfig),
    max_iterations: record.maxIterations,
    max_cost_per_run: record.maxCostPerRun,
    timeout_s: record.timeoutS,
    enabled: record.enabled,
    next_run_at: record.nextRunAt,
    last_run_at: record.lastRunAt,
    last_status: record.lastStatus as ScheduledTask["last_status"],
    run_count: record.runCount,
    failure_count: record.failureCount,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
    schedule_description: record.scheduleDescription ?? null,
    next_runs: record.nextRuns ?? [],
  }
}

export function toTaskRun(record: protocol.TaskRunRecord): TaskRun {
  return {
    id: record.id,
    task_id: record.taskId,
    conversation_id: record.conversationId,
    run_id: record.runId,
    status: record.status as TaskRun["status"],
    trigger_source: record.triggerSource as TaskRun["trigger_source"],
    prompt: record.prompt,
    output: record.output,
    error: record.error,
    skip_reason: record.skipReason,
    approval_policy: record.approvalPolicy as TaskRun["approval_policy"],
    approval_reason: record.approvalReason,
    usage: asObject(record.usage),
    duration_ms: record.durationMs,
    delivery_status: asObject(record.deliveryStatus) as Record<string, string> | null,
    delivered_at: record.deliveredAt,
    is_read: record.isRead,
    started_at: record.startedAt,
    finished_at: record.finishedAt,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toTaskRunDetail(record: protocol.TaskRunDetailRecord): TaskRunDetail {
  return {
    ...toTaskRun(record.run),
    messages: record.messages.map((message) => ({
      id: message.id,
      role: message.role,
      content: message.content,
      tool_calls: Array.isArray(message.toolCalls)
        ? (message.toolCalls as unknown as Record<string, unknown>[])
        : null,
      tool_result: asObject(message.toolResult),
      created_at: message.createdAt,
    })),
  }
}

export function toTaskInbox(record: protocol.TaskInboxResult): TaskInbox {
  return { unread_count: record.unreadCount, runs: record.runs.map(toTaskRun) }
}

export function toParseCron(record: protocol.ParseCronResult): ParseCronResponse {
  return {
    cron_expression: record.cronExpression,
    description: record.description,
    next_runs: record.nextRuns,
    detail: record.detail,
  }
}

// --- webhooks ---

export function toWebhookEndpoint(record: protocol.WebhookEndpointRecord): WebhookEndpoint {
  return {
    id: record.id,
    user_id: record.userId,
    name: record.name,
    hook_id: record.hookId,
    // The canonical record never exposes the signing secret (M11 gap).
    secret: "",
    source_type: record.sourceType as WebhookEndpoint["source_type"],
    event_filter: asOptionalStringArray(record.eventFilter),
    task_id: record.taskId,
    prompt_template: record.promptTemplate,
    enabled: record.enabled,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
    url_path: `/api/webhooks/inbound/${record.hookId}`,
  }
}

export function toWebhookEvent(record: protocol.WebhookEventRecord): WebhookEvent {
  return {
    id: record.id,
    endpoint_id: record.endpointId,
    event_type: record.eventType,
    payload: asObject(record.payload),
    signature_valid: record.signatureValid,
    status: record.status as WebhookEvent["status"],
    task_run_id: record.taskRunId,
    error: record.error,
    received_at: record.receivedAt,
    created_at: record.createdAt,
  }
}

// --- wiki ---

export function toWikiArticle(record: protocol.WikiArticleRecord): WikiArticle {
  return {
    id: record.id,
    title: record.title,
    content: record.content,
    category: record.category,
    tags: asStringArray(record.tags),
    source: record.source,
    source_memory_id: record.sourceMemoryId,
    is_pinned: record.isPinned,
    is_archived: record.isArchived,
    version: record.version,
    created_at: record.createdAt,
    updated_at: record.updatedAt,
  }
}

export function toWikiStats(record: protocol.WikiStatsRecord): {
  total_articles: number
  by_category: Record<string, number>
} {
  return { total_articles: record.total, by_category: record.byCategory }
}

// --- inspector ---

export function toReplayResponse(record: protocol.ReplayResult): ReplayResponse {
  return {
    new_run_id: record.newRunId,
    original_run_id: record.originalRunId,
    status: record.status,
  }
}

/** Group the seq-ordered event entries into the Python inspector iterations. */
export function toRunTimeline(record: protocol.TimelineRecord): RunTimeline {
  const iterations: IterationDetail[] = []
  let current: IterationDetail | null = null
  for (const entry of record.entries) {
    if (entry.kind === "llm_call_complete") {
      const payload = asObject(entry.payload)
      current = {
        iteration: iterations.length + 1,
        duration_ms: entry.durationMs,
        usage: asObject(payload?.usage),
        model: entry.title ?? asString(payload?.model),
        tool_calls: [],
        finish_reason: asString(payload?.finish_reason),
      }
      iterations.push(current)
    } else if (entry.kind === "tool_call_start" && current) {
      const payload = asObject(entry.payload)
      current.tool_calls.push({
        id: asString(payload?.id),
        name: entry.title ?? asString(payload?.name) ?? "tool",
        arguments: asObject(payload?.arguments) ?? {},
      })
    }
  }
  const finalize = record.entries.find((entry) => entry.kind === "finish")
  // Python attaches the terminal finish reason to the last iteration.
  const finalReason = finalize ? asString(asObject(finalize.payload)?.reason) ?? finalize.title : null
  if (finalReason && iterations.length > 0) {
    iterations[iterations.length - 1].finish_reason = finalReason
  }
  const run: RunDetail = {
    id: record.runId,
    conversation_id: record.conversationId,
    status: record.status,
    model: iterations.find((iteration) => iteration.model)?.model ?? null,
    iterations: iterations.length,
    usage: asObject(record.usage),
    finish_reason: finalReason,
    error: record.error,
    started_at: record.startedAt ?? "",
    finished_at: record.finishedAt,
    created_at: record.startedAt ?? "",
    updated_at: record.finishedAt ?? record.startedAt ?? "",
    config: null,
    checkpoint: null,
    events: record.entries.map(
      (entry): RunEventOut => ({
        id: entry.index,
        run_id: record.runId,
        seq: entry.index,
        kind: entry.kind,
        payload: asObject(entry.payload),
        created_at: entry.occurredAt,
      })
    ),
  }
  return { run, iterations, total_duration_ms: record.totalDurationMs }
}

export function toToolCatalogItem(record: protocol.ToolCatalogRecord): ToolCatalogItem {
  return {
    name: record.name,
    description: record.description,
    dangerous: record.dangerous,
    capabilities: record.capabilities,
    parameters: asObject(record.parameters) ?? {},
    is_macro: record.isMacro,
  }
}

export function toTaskTemplate(record: protocol.TaskTemplateRecord): TaskTemplate {
  return {
    slug: record.slug,
    name: record.name,
    description: record.description,
    prompt: record.prompt,
    cron_expression: record.cronExpression,
    tools_whitelist: record.toolsWhitelist,
    max_iterations: record.maxIterations,
    delivery_channels: record.deliveryChannels,
  }
}

export function toSchedulerStatus(record: protocol.SchedulerStatusRecord): SchedulerStatus {
  return {
    enabled: record.enabled,
    running: record.running,
    timezone: record.timezone,
    max_concurrent_tasks: record.maxConcurrentTasks,
    jobs: record.jobs.map((job) => ({
      id: job.id,
      name: job.name,
      next_run_time: job.nextRunTime,
    })),
  }
}

export function toRunComparison(record: protocol.RunComparisonRecord): RunComparison {
  const left = toRunTimeline(record.left)
  const right = toRunTimeline(record.right)
  return {
    run_a: left.run,
    run_b: right.run,
    delta_tokens: record.deltas.totalTokens ?? 0,
    delta_cost_usd: record.deltas.costUsd,
    delta_iterations: right.run.iterations - left.run.iterations,
    delta_duration_ms: record.deltas.durationMs,
    iterations_a: left.iterations,
    iterations_b: right.iterations,
  }
}
