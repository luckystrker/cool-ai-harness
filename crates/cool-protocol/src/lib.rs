use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const PROTOCOL_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;

mod families;

pub use families::*;

pub type Extensions = BTreeMap<String, Value>;

macro_rules! fixed_wire_string {
    ($name:ident, $value:literal, $typescript:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, TS)]
        #[ts(type = $typescript)]
        pub struct $name;

        impl $name {
            pub const VALUE: Self = Self;
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str($value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let actual = String::deserialize(deserializer)?;
                if actual == $value {
                    Ok(Self)
                } else {
                    Err(serde::de::Error::custom(concat!("expected literal ", $value)))
                }
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({"type": "string", "const": $value})
            }
        }
    };
}

fixed_wire_string!(JsonRpcV2, "2.0", "\"2.0\"");
fixed_wire_string!(CoolCommandMethod, "cool.command", "\"cool.command\"");
fixed_wire_string!(RunEventMethod, "run.event", "\"run.event\"");

#[derive(Clone, Copy, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct V1Version(#[schemars(range(min = 1, max = 1))] u32);

impl V1Version {
    pub const VALUE: Self = Self(1);
}

impl<'de> Deserialize<'de> for V1Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == 1 {
            Ok(Self::VALUE)
        } else {
            Err(serde::de::Error::custom(
                "only App Protocol version 1 is supported",
            ))
        }
    }
}

/// Maximum accepted idempotency-key length.
pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 256;

#[derive(Clone, Debug, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(#[schemars(length(min = 1, max = 256))] String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty() {
            Err("idempotency key must not be empty")
        } else if value.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS {
            Err("idempotency key is too long")
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ActorRef {
    pub id: String,
    pub kind: ActorKind,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ActorKind {
    LocalUser,
    ServerUser,
    TelegramUser,
    System,
    Worker,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CommandEnvelope {
    #[ts(type = "1")]
    pub protocol_version: V1Version,
    pub command_id: String,
    pub command: Command,
}

/// A server-normalized command. Transport authentication supplies `actor`;
/// it is intentionally absent from the client-deserializable envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorizedCommand {
    pub actor: ActorRef,
    pub command: CommandEnvelope,
}

// The protocol enums mirror the wire schema; boxing variants would only churn
// every transport match without changing the JSON representation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(tag = "method", content = "params", deny_unknown_fields)]
#[ts(export)]
pub enum Command {
    #[serde(rename = "initialize")]
    Initialize(InitializeParams),
    #[serde(rename = "session.create")]
    SessionCreate(SessionCreateParams),
    #[serde(rename = "session.load")]
    SessionLoad(SessionLoadParams),
    #[serde(rename = "session.prompt")]
    SessionPrompt(SessionPromptParams),
    #[serde(rename = "session.list")]
    SessionList(SessionListParams),
    #[serde(rename = "session.history")]
    SessionHistory(SessionHistoryParams),
    #[serde(rename = "session.fork")]
    SessionFork(SessionForkParams),
    #[serde(rename = "session.rewind")]
    SessionRewind(SessionRewindParams),
    #[serde(rename = "session.for_conversation")]
    SessionForConversation(SessionForConversationParams),
    #[serde(rename = "session.runs")]
    SessionRuns(SessionRunsParams),
    #[serde(rename = "session.steer")]
    SessionSteer(SessionSteerParams),
    #[serde(rename = "run.cancel")]
    RunCancel(RunCancelParams),
    #[serde(rename = "run.events")]
    RunEvents(RunEventsParams),
    #[serde(rename = "run.subscribe")]
    RunSubscribe(RunSubscribeParams),
    #[serde(rename = "approval.resolve")]
    ApprovalResolve(ApprovalResolveParams),
    #[serde(rename = "policy.rules_list")]
    PolicyRulesList(PolicyRulesListParams),
    #[serde(rename = "policy.rule_add")]
    PolicyRuleAdd(PolicyRuleAddParams),
    #[serde(rename = "policy.rule_delete")]
    PolicyRuleDelete(PolicyRuleDeleteParams),
    #[serde(rename = "status.get")]
    StatusGet(StatusGetParams),
    #[serde(rename = "tools.list")]
    ToolsList(EmptyParams),
    #[serde(rename = "extensions.status")]
    ExtensionsStatus(EmptyParams),
    #[serde(rename = "extensions.plugin_enabled")]
    ExtensionsPluginEnabled(PluginEnabledParams),
    #[serde(rename = "extensions.hook_review")]
    ExtensionsHookReview(HookReviewParams),
    #[serde(rename = "settings.system_prompt")]
    SettingsSystemPrompt(EmptyParams),
    #[serde(rename = "settings.system_prompt_set")]
    SettingsSystemPromptSet(SystemPromptSetParams),
    #[serde(rename = "mcp.list_servers")]
    McpListServers(EmptyParams),
    #[serde(rename = "mcp.add_server")]
    McpAddServer(McpAddServerParams),
    #[serde(rename = "mcp.update_server")]
    McpUpdateServer(McpUpdateServerParams),
    #[serde(rename = "mcp.remove_server")]
    McpRemoveServer(McpServerNameParams),
    #[serde(rename = "mcp.connect")]
    McpConnect(McpServerNameParams),
    #[serde(rename = "mcp.disconnect")]
    McpDisconnect(McpServerNameParams),
    #[serde(rename = "mcp.health")]
    McpHealth(McpServerNameParams),
    #[serde(rename = "mcp.list_tools")]
    McpListTools(EmptyParams),
    #[serde(rename = "mcp.reconnect_all")]
    McpReconnectAll(EmptyParams),
    #[serde(rename = "mcp.store_search")]
    McpStoreSearch(McpStoreSearchParams),
    #[serde(rename = "mcp.store_popular")]
    McpStorePopular(McpStorePopularParams),
    #[serde(rename = "mcp.store_install")]
    McpStoreInstall(McpStoreInstallParams),
    #[serde(rename = "skills.list")]
    SkillsList(SkillListParams),
    #[serde(rename = "skills.create")]
    SkillsCreate(SkillCreateParams),
    #[serde(rename = "skills.delete")]
    SkillsDelete(SkillDeleteParams),
    #[serde(rename = "conversations.list")]
    ConversationsList(ConversationListParams),
    #[serde(rename = "conversations.create")]
    ConversationsCreate(ConversationCreateParams),
    #[serde(rename = "conversations.get")]
    ConversationsGet(LegacyIdParams),
    #[serde(rename = "conversations.messages")]
    ConversationsMessages(ConversationMessagesParams),
    #[serde(rename = "conversations.update")]
    ConversationsUpdate(ConversationUpdateParams),
    #[serde(rename = "conversations.delete")]
    ConversationsDelete(IdempotentIdParams),
    #[serde(rename = "conversations.compact")]
    ConversationsCompact(IdempotentIdParams),
    #[serde(rename = "conversations.approvals")]
    ConversationsApprovals(ConversationApprovalsParams),
    #[serde(rename = "conversations.search")]
    ConversationsSearch(ConversationSearchParams),
    #[serde(rename = "conversations.bulk")]
    ConversationsBulk(ConversationBulkParams),
    #[serde(rename = "runs.list")]
    RunsList(RunListParams),
    #[serde(rename = "runs.get")]
    RunsGet(LegacyIdParams),
    #[serde(rename = "runs.events")]
    RunsEvents(RunEventsLegacyParams),
    #[serde(rename = "runs.cancel")]
    RunsCancel(IdempotentIdParams),
    #[serde(rename = "providers.list")]
    ProvidersList(ProviderListParams),
    #[serde(rename = "providers.create")]
    ProvidersCreate(ProviderCreateParams),
    #[serde(rename = "providers.get")]
    ProvidersGet(LegacyIdParams),
    #[serde(rename = "providers.update")]
    ProvidersUpdate(ProviderUpdateParams),
    #[serde(rename = "providers.delete")]
    ProvidersDelete(IdempotentIdParams),
    #[serde(rename = "providers.models")]
    ProvidersModels(LegacyIdParams),
    #[serde(rename = "providers.list_models")]
    ProvidersListModels(LegacyIdParams),
    #[serde(rename = "providers.preview_models")]
    ProvidersPreviewModels(ProvidersPreviewModelsParams),
    #[serde(rename = "providers.oauth_start")]
    ProvidersOauthStart(ProvidersOauthStartParams),
    #[serde(rename = "providers.oauth_complete")]
    ProvidersOauthComplete(ProvidersOauthCompleteParams),
    #[serde(rename = "memory.list")]
    MemoryList(MemoryListParams),
    #[serde(rename = "memory.get")]
    MemoryGet(LegacyIdParams),
    #[serde(rename = "memory.create")]
    MemoryCreate(MemoryCreateParams),
    #[serde(rename = "memory.update")]
    MemoryUpdate(MemoryUpdateParams),
    #[serde(rename = "memory.delete")]
    MemoryDelete(MemoryDeleteParams),
    #[serde(rename = "memory.pending")]
    MemoryPending(MemoryPendingParams),
    #[serde(rename = "memory.confirm")]
    MemoryConfirm(IdempotentIdParams),
    #[serde(rename = "memory.reject")]
    MemoryReject(IdempotentIdParams),
    #[serde(rename = "memory.pin")]
    MemoryPin(MemoryPinParams),
    #[serde(rename = "memory.explain")]
    MemoryExplain(LegacyIdParams),
    #[serde(rename = "memory.extract")]
    MemoryExtract(MemoryExtractParams),
    #[serde(rename = "memory.episodes")]
    MemoryEpisodes(MemoryEpisodesParams),
    #[serde(rename = "memory.stats")]
    MemoryStats(EmptyParams),
    #[serde(rename = "entities.list")]
    EntitiesList(EntityListParams),
    #[serde(rename = "entities.get")]
    EntitiesGet(LegacyIdParams),
    #[serde(rename = "entities.create")]
    EntitiesCreate(EntityCreateParams),
    #[serde(rename = "entities.update")]
    EntitiesUpdate(EntityUpdateParams),
    #[serde(rename = "entities.delete")]
    EntitiesDelete(IdempotentIdParams),
    #[serde(rename = "plans.list")]
    PlansList(PlanListParams),
    #[serde(rename = "plans.get")]
    PlansGet(PlanIdParams),
    #[serde(rename = "plans.update")]
    PlansUpdate(PlanUpdateParams),
    #[serde(rename = "plans.approve")]
    PlansApprove(PlanApproveParams),
    #[serde(rename = "plans.execute")]
    PlansExecute(IdempotentPlanIdParams),
    #[serde(rename = "plans.cancel")]
    PlansCancel(IdempotentPlanIdParams),
    #[serde(rename = "plans.templates_list")]
    PlansTemplatesList(EmptyParams),
    #[serde(rename = "plans.templates_create")]
    PlansTemplatesCreate(PlanTemplateCreateParams),
    #[serde(rename = "plans.templates_delete")]
    PlansTemplatesDelete(IdempotentIdParams),
    #[serde(rename = "subagents.roles_list")]
    SubagentsRolesList(EmptyParams),
    #[serde(rename = "subagents.roles_get")]
    SubagentsRolesGet(LegacyIdParams),
    #[serde(rename = "subagents.roles_create")]
    SubagentsRolesCreate(SubagentRoleCreateParams),
    #[serde(rename = "subagents.roles_update")]
    SubagentsRolesUpdate(SubagentRoleUpdateParams),
    #[serde(rename = "subagents.roles_delete")]
    SubagentsRolesDelete(IdempotentIdParams),
    #[serde(rename = "subagents.launch")]
    SubagentsLaunch(SubagentLaunchParams),
    #[serde(rename = "subagents.launch_batch")]
    SubagentsLaunchBatch(SubagentLaunchBatchParams),
    #[serde(rename = "subagents.runs_list")]
    SubagentsRunsList(SubagentRunListParams),
    #[serde(rename = "subagents.runs_get")]
    SubagentsRunsGet(LegacyIdParams),
    #[serde(rename = "subagents.runs_cancel")]
    SubagentsRunsCancel(IdempotentIdParams),
    #[serde(rename = "subagents.runs_delete")]
    SubagentsRunsDelete(IdempotentIdParams),
    #[serde(rename = "inspector.timeline")]
    InspectorTimeline(LegacyIdParams),
    #[serde(rename = "inspector.compare")]
    InspectorCompare(InspectorCompareParams),
    #[serde(rename = "inspector.replay")]
    InspectorReplay(ReplayParams),
    #[serde(rename = "budgets.get")]
    BudgetsGet(EmptyParams),
    #[serde(rename = "budgets.update")]
    BudgetsUpdate(BudgetUpdateParams),
    #[serde(rename = "budgets.override_set")]
    BudgetsOverrideSet(BudgetOverrideParams),
    #[serde(rename = "budgets.override_clear")]
    BudgetsOverrideClear(IdempotentParams),
    #[serde(rename = "budgets.spend")]
    BudgetsSpend(BudgetSpendParams),
    #[serde(rename = "artifacts.list")]
    ArtifactsList(ArtifactListParams),
    #[serde(rename = "artifacts.get")]
    ArtifactsGet(ArtifactIdParams),
    #[serde(rename = "artifacts.delete")]
    ArtifactsDelete(IdempotentIdParams),
    #[serde(rename = "workspace.git_info")]
    WorkspaceGitInfo(WorkspacePathParams),
    #[serde(rename = "workspace.directories")]
    WorkspaceDirectories(WorkspaceOptionalPathParams),
    #[serde(rename = "workspace.recent")]
    WorkspaceRecent(EmptyParams),
    #[serde(rename = "workspace.git_status")]
    WorkspaceGitStatus(WorkspacePathParams),
    #[serde(rename = "workspace.git_log")]
    WorkspaceGitLog(WorkspaceGitLogParams),
    #[serde(rename = "workspace.git_branches")]
    WorkspaceGitBranches(WorkspacePathParams),
    #[serde(rename = "workspace.git_checkout")]
    WorkspaceGitCheckout(WorkspaceGitCheckoutParams),
    #[serde(rename = "profiles.list")]
    ProfilesList(ProfileListParams),
    #[serde(rename = "profiles.get")]
    ProfilesGet(LegacyIdParams),
    #[serde(rename = "profiles.create")]
    ProfilesCreate(ProfileCreateParams),
    #[serde(rename = "profiles.update")]
    ProfilesUpdate(ProfileUpdateParams),
    #[serde(rename = "profiles.delete")]
    ProfilesDelete(IdempotentIdParams),
    #[serde(rename = "profiles.seed")]
    ProfilesSeed(IdempotentParams),
    #[serde(rename = "profiles.clone")]
    ProfilesClone(IdempotentIdParams),
    #[serde(rename = "profiles.playground")]
    ProfilesPlayground(ProfilePlaygroundParams),
    #[serde(rename = "analytics.summary")]
    AnalyticsSummary(AnalyticsDaysParams),
    #[serde(rename = "analytics.spend_over_time")]
    AnalyticsSpendOverTime(AnalyticsBucketParams),
    #[serde(rename = "analytics.spend_by_model")]
    AnalyticsSpendByModel(AnalyticsDaysParams),
    #[serde(rename = "analytics.top_tools")]
    AnalyticsTopTools(AnalyticsTopToolsParams),
    #[serde(rename = "analytics.latency")]
    AnalyticsLatency(AnalyticsBucketParams),
    #[serde(rename = "analytics.call_history")]
    AnalyticsCallHistory(AnalyticsCallHistoryParams),
    #[serde(rename = "analytics.memory_activity")]
    AnalyticsMemoryActivity(AnalyticsBucketParams),
    #[serde(rename = "tasks.list")]
    TasksList(TaskListParams),
    #[serde(rename = "tasks.get")]
    TasksGet(LegacyIdParams),
    #[serde(rename = "tasks.create")]
    TasksCreate(TaskCreateParams),
    #[serde(rename = "tasks.update")]
    TasksUpdate(TaskUpdateParams),
    #[serde(rename = "tasks.delete")]
    TasksDelete(IdempotentIdParams),
    #[serde(rename = "tasks.run")]
    TasksRun(IdempotentIdParams),
    #[serde(rename = "tasks.runs_list")]
    TasksRunsList(TaskRunsParams),
    #[serde(rename = "tasks.runs_get")]
    TasksRunsGet(LegacyIdParams),
    #[serde(rename = "tasks.runs_cancel")]
    TasksRunsCancel(IdempotentIdParams),
    #[serde(rename = "tasks.runs_read")]
    TasksRunsRead(TaskRunReadParams),
    #[serde(rename = "tasks.inbox")]
    TasksInbox(TaskInboxParams),
    #[serde(rename = "tasks.scheduler")]
    TasksScheduler(EmptyParams),
    #[serde(rename = "tasks.templates")]
    TasksTemplates(EmptyParams),
    #[serde(rename = "tasks.parse_cron")]
    TasksParseCron(ParseCronParams),
    #[serde(rename = "rss.subscriptions_list")]
    RssSubscriptionsList(RssSubscriptionListParams),
    #[serde(rename = "rss.subscribe")]
    RssSubscribe(RssSubscribeParams),
    #[serde(rename = "rss.unsubscribe")]
    RssUnsubscribe(IdempotentIdParams),
    #[serde(rename = "rss.entries_list")]
    RssEntriesList(RssEntriesParams),
    #[serde(rename = "rss.entries_all")]
    RssEntriesAll(RssAllEntriesParams),
    #[serde(rename = "rss.entry_read")]
    RssEntryRead(RssEntryReadParams),
    #[serde(rename = "rss.fetch_now")]
    RssFetchNow(IdempotentIdParams),
    #[serde(rename = "webhooks.list")]
    WebhooksList(EmptyParams),
    #[serde(rename = "webhooks.get")]
    WebhooksGet(LegacyIdParams),
    #[serde(rename = "webhooks.create")]
    WebhooksCreate(WebhookCreateParams),
    #[serde(rename = "webhooks.update")]
    WebhooksUpdate(WebhookUpdateParams),
    #[serde(rename = "webhooks.delete")]
    WebhooksDelete(IdempotentIdParams),
    #[serde(rename = "webhooks.events")]
    WebhooksEvents(WebhookEventsParams),
    #[serde(rename = "webhooks.replay")]
    WebhooksReplay(WebhookReplayParams),
    #[serde(rename = "wiki.list")]
    WikiList(WikiListParams),
    #[serde(rename = "wiki.search")]
    WikiSearch(WikiSearchParams),
    #[serde(rename = "wiki.get")]
    WikiGet(LegacyIdParams),
    #[serde(rename = "wiki.create")]
    WikiCreate(WikiCreateParams),
    #[serde(rename = "wiki.update")]
    WikiUpdate(WikiUpdateParams),
    #[serde(rename = "wiki.delete")]
    WikiDelete(IdempotentIdParams),
    #[serde(rename = "wiki.categories")]
    WikiCategories(EmptyParams),
    #[serde(rename = "wiki.stats")]
    WikiStats(EmptyParams),
    #[serde(rename = "wiki.promote")]
    WikiPromote(WikiPromoteParams),
    #[serde(rename = "research.list")]
    ResearchList(ResearchListParams),
    #[serde(rename = "research.get")]
    ResearchGet(LegacyIdParams),
    #[serde(rename = "research.create")]
    ResearchCreate(ResearchCreateParams),
    #[serde(rename = "research.cancel")]
    ResearchCancel(IdempotentIdParams),
    #[serde(rename = "research.rerun")]
    ResearchRerun(ResearchRerunParams),
    #[serde(rename = "constructor.macros")]
    ConstructorMacros(ConstructorMacroListParams),
    #[serde(rename = "constructor.macros_create")]
    ConstructorMacrosCreate(MacroCreateParams),
    #[serde(rename = "constructor.macros_update")]
    ConstructorMacrosUpdate(MacroUpdateParams),
    #[serde(rename = "constructor.macros_delete")]
    ConstructorMacrosDelete(IdempotentIdParams),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct InitializeParams {
    pub client_name: String,
    pub client_version: String,
    pub supported_protocol_versions: Vec<u32>,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionCreateParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub title: Option<String>,
    pub project_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionLoadParams {
    pub session_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionPromptParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub session_id: String,
    pub content: Vec<ContentPart>,
    pub model: Option<String>,
    /// Planning mode: the runtime owns the system prompt (a canonical planning
    /// directive) and marks the run mode as `plan`, so the model produces a
    /// plan through the trusted `update_plan` tool instead of executing.
    #[serde(default)]
    pub plan_mode: bool,
    /// Long-running task mode: the run is marked `long_task` and the existing
    /// task progress file (`.cool/task/progress.md`, maintained by the bundled
    /// long-running-task skill) is injected into the system prompt so the
    /// agent resumes from tracked state. Ignored in planning mode.
    #[serde(default)]
    pub long_task_mode: bool,
    /// Caller-supplied system prompt. Ignored in planning mode, where the
    /// runtime owns the system prompt so a caller cannot steer the plan.
    pub system_prompt: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum ContentPart {
    Text {
        text: String,
    },
    Artifact {
        #[serde(rename = "artifactId")]
        #[ts(rename = "artifactId")]
        artifact_id: String,
    },
    Image {
        #[serde(rename = "artifactId")]
        #[ts(rename = "artifactId")]
        artifact_id: String,
        #[serde(rename = "mediaType")]
        #[ts(rename = "mediaType")]
        media_type: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionListParams {
    pub project_key: Option<String>,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionHistoryParams {
    pub session_id: String,
    pub limit: u16,
    /// Exclusive, opaque durable cursor: only history derived from events
    /// appended before `before_cursor` is returned. Older pages pass the
    /// `next_cursor` of the previous page; absent, the newest page is returned.
    #[ts(type = "number | null")]
    pub before_cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionForkParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub session_id: String,
    pub title: Option<String>,
    /// Fork point: only events at or below this cursor are copied. The cursor
    /// is the `HistoryItem.cursor` space (`rust_events.rowid`), the same
    /// durable cursor `session.history` returns and renders in the UI.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub up_to_cursor: Option<u64>,
    /// Alternate bound in the events' own `seq` space (`e.seq` <= N). An alias
    /// for `up_to_cursor` for callers that page `run.events` by seq; when both
    /// bounds are set, both apply.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub up_to_event_seq: Option<u64>,
}

/// Rewind a session to an earlier point: the previous runs are superseded
/// (`rewound` status, append-only — their events stay durable) and a fresh
/// seed run carries the retained history prefix up to `to_cursor`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionRewindParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub session_id: String,
    /// Durable cursor the session rewinds to (`HistoryItem.cursor`, the
    /// `rust_events.rowid`). History visible after the rewind is exactly the
    /// prefix of history events at or below this cursor.
    #[ts(type = "number")]
    pub to_cursor: u64,
    #[serde(default)]
    pub reason: Option<String>,
    /// Restore the workspace to the filesystem checkpoint recorded nearest
    /// to `to_cursor` (P2.18). Without a recorded checkpoint ref the rewind
    /// still lands and the result reports `workspaceRestored: false`.
    #[serde(default)]
    pub restore_workspace: Option<bool>,
}

/// Binds one legacy conversation to a durable Rust session, importing the
/// legacy transcript once so chat turns run on the canonical event model.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionForConversationParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub conversation_id: i64,
    /// Bind an existing actor-owned session instead of creating one (fork
    /// flows): the conversation links to that session and no transcript is
    /// imported. The session must not already be linked to a conversation.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// List durable runs of one session, newest first.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionRunsParams {
    pub session_id: String,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionSteerParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub run_id: String,
    pub content: Vec<ContentPart>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunCancelParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub run_id: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunEventsParams {
    pub run_id: String,
    #[ts(type = "number | null")]
    pub after_seq: Option<u64>,
    pub limit: u16,
}

/// Subscribes the calling connection to another connection's run so live
/// `run.event` notifications fan out to every subscriber, not only the
/// connection that started the run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunSubscribeParams {
    pub run_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalResolveParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    pub approval_id: String,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub decision: ApprovalDecision,
    /// Persist a rule for the approved call: `"session"` keeps it in memory
    /// for the run, `"project"` writes `<workspace>/.cool/policy.json`,
    /// `"user"` stores it in the durable `policy_rules` table.
    #[serde(default)]
    pub remember: Option<String>,
    /// Rule payload for `remember`; when absent the server derives one from
    /// the tool call (same shape as `ToolApprovalRequired.suggestedRule`).
    #[serde(default)]
    pub rule: Option<PolicyRuleRecord>,
    /// Free-form answer for `breakpointType: "question"` asks (the `ask_user`
    /// tool): an option id/string or arbitrary JSON. Ignored for plain
    /// allow/deny approvals.
    #[serde(default)]
    pub answer: Option<serde_json::Value>,
}

/// Wire mirror of `cool_security::PolicyRule` — kept as plain strings so the
/// protocol crate carries no cool-security dependency.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PolicyRuleRecord {
    /// Qualified id (`"user:3"`, `"project:1"`, `"session:0"`) for deletes.
    #[serde(default)]
    pub id: Option<String>,
    /// Tool name, or `"*"` for every tool.
    pub tool: String,
    /// `command` | `path_glob` | `domain` | `any`.
    pub kind: String,
    #[serde(default)]
    pub pattern: String,
    /// `allow` | `ask` | `deny`.
    pub decision: String,
    /// `session` | `project` | `user`.
    pub scope: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PolicyRulesListParams {
    /// Optional scope filter (`session|project|user`); absent lists all.
    #[serde(default)]
    pub scope: Option<String>,
    /// Session rules of this run when `scope = "session"`.
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PolicyRulesListResult {
    pub rules: Vec<PolicyRuleRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PolicyRuleAddParams {
    pub rule: PolicyRuleRecord,
    /// Session rules attach to a live run; required when `scope = "session"`.
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PolicyRuleDeleteParams {
    #[ts(type = "string")]
    pub idempotency_key: IdempotencyKey,
    /// Qualified rule id from `policy.rules_list` (`"user:3"`, `"project:1"`,
    /// `"session:0"`).
    pub rule_id: String,
    /// Required when deleting a `session:` rule.
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PolicyRuleDeleteResult {
    pub deleted: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StatusGetParams {}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EventCursor {
    pub run_id: String,
    #[ts(type = "number | null")]
    pub after_seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EventPage {
    pub events: Vec<EventEnvelope>,
    pub next_cursor: Option<EventCursor>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProtocolError {
    pub rpc_code: i32,
    pub cool_code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub safe_details: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EventEnvelope {
    pub event_id: String,
    #[ts(type = "1")]
    pub schema_version: V1Version,
    pub session_id: String,
    pub run_id: String,
    pub item_id: Option<String>,
    #[ts(type = "number")]
    pub seq: u64,
    pub occurred_at: String,
    pub actor: ActorRef,
    pub source: String,
    pub causation_id: Option<String>,
    pub correlation_id: Option<String>,
    pub event: CanonicalEvent,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: Extensions,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(tag = "kind", content = "payload", deny_unknown_fields)]
#[ts(export)]
pub enum CanonicalEvent {
    #[serde(rename = "session.created")]
    SessionCreated(SessionEvent),
    #[serde(rename = "session.updated")]
    SessionUpdated(SessionEvent),
    #[serde(rename = "session.compacted")]
    SessionCompacted(SessionCompacted),
    #[serde(rename = "run.started")]
    RunStarted(RunStarted),
    #[serde(rename = "run.completed")]
    RunCompleted(RunTerminal),
    #[serde(rename = "run.failed")]
    RunFailed(RunTerminal),
    #[serde(rename = "run.cancelled")]
    RunCancelled(RunTerminal),
    /// Marker event appended to the seed run a `session.rewind` creates: the
    /// session's visible history was reset to the prefix ending at `cursor`.
    /// It does not change run status — the superseded runs carry `rewound`.
    #[serde(rename = "run.rewound")]
    RunRewound(RunRewound),
    #[serde(rename = "item.started")]
    ItemStarted(ItemEvent),
    #[serde(rename = "item.updated")]
    ItemUpdated(ItemEvent),
    #[serde(rename = "item.completed")]
    ItemCompleted(ItemEvent),
    #[serde(rename = "content.delta")]
    ContentDelta(TextDelta),
    #[serde(rename = "reasoning.delta")]
    ReasoningDelta(TextDelta),
    #[serde(rename = "tool.requested")]
    ToolRequested(ToolRequested),
    #[serde(rename = "tool.approval_required")]
    ToolApprovalRequired(Box<ToolApprovalRequired>),
    #[serde(rename = "tool.approval_resolved")]
    ToolApprovalResolved(ToolApprovalResolved),
    #[serde(rename = "tool.started")]
    ToolStarted(ToolLifecycle),
    #[serde(rename = "tool.completed")]
    ToolCompleted(ToolCompleted),
    #[serde(rename = "tool.failed")]
    ToolFailed(ToolFailed),
    #[serde(rename = "plan.created")]
    PlanCreated(PlanCreated),
    #[serde(rename = "plan.step_started")]
    PlanStepStarted(PlanStep),
    #[serde(rename = "plan.step_completed")]
    PlanStepCompleted(PlanStep),
    #[serde(rename = "plan.progress")]
    PlanProgress(PlanProgress),
    #[serde(rename = "artifact.created")]
    ArtifactCreated(ArtifactCreated),
    #[serde(rename = "usage.updated")]
    UsageUpdated(UsageUpdated),
    #[serde(rename = "budget.warning")]
    BudgetWarning(BudgetEvent),
    #[serde(rename = "budget.exceeded")]
    BudgetExceeded(BudgetEvent),
    #[serde(rename = "subagent.started")]
    SubagentStarted(SubagentEvent),
    #[serde(rename = "subagent.progress")]
    SubagentProgress(SubagentProgress),
    #[serde(rename = "subagent.completed")]
    SubagentCompleted(SubagentEvent),
    #[serde(rename = "subagent.failed")]
    SubagentFailed(SubagentEvent),
    #[serde(rename = "worker.started")]
    WorkerStarted(WorkerEvent),
    #[serde(rename = "worker.failed")]
    WorkerFailed(WorkerEvent),
    #[serde(rename = "worker.restarted")]
    WorkerRestarted(WorkerEvent),
    #[serde(rename = "plugin.status")]
    PluginStatus(PluginStatusEvent),
    #[serde(rename = "research.stage")]
    ResearchStage(ResearchStage),
    #[serde(rename = "research.started")]
    ResearchStarted(ResearchStarted),
    #[serde(rename = "research.source_found")]
    ResearchSourceFound(ResearchSource),
    #[serde(rename = "research.subquestion_started")]
    ResearchSubquestionStarted(ResearchSubquestion),
    #[serde(rename = "research.subquestion_completed")]
    ResearchSubquestionCompleted(ResearchSubquestion),
    #[serde(rename = "research.completed")]
    ResearchCompleted(ResearchTerminal),
    #[serde(rename = "research.failed")]
    ResearchFailed(ResearchTerminal),
    #[serde(rename = "research.cancelled")]
    ResearchCancelled(ResearchTerminal),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionEvent {
    pub title: Option<String>,
    pub project_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionCompacted {
    pub retained_items: u32,
    pub summary_item_id: Option<String>,
    /// Rolling summary text, when the compaction produced one. `session.history`
    /// projects it as a summary item so the chat can render the compacted block.
    #[serde(default)]
    pub summary: Option<String>,
    /// Cursor of the newest compacted history item. Items with a cursor `<=`
    /// this value are covered by `summary`.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub compact_up_to_cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunStarted {
    pub model: Option<String>,
    pub mode: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunTerminal {
    pub reason: String,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunRewound {
    /// Durable cursor (`rust_events.rowid`) the session rewound to.
    #[ts(type = "number")]
    pub cursor: u64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ItemEvent {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolRequested>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TextDelta {
    pub text: String,
    pub channel: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolRequested {
    pub call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolApprovalRequired {
    pub call_id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
    pub reason: String,
    pub approval_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub breakpoint_type: Option<String>,
    pub result_preview: Option<String>,
    pub current_content: Option<String>,
    /// The exec/policy rule that produced this ask (`None` when the ask came
    /// from the capability fallback).
    #[serde(default)]
    pub matched_rule: Option<String>,
    /// A rule the client can persist via `approval.resolve {remember}` to
    /// stop prompting for equivalent calls.
    #[serde(default)]
    pub suggested_rule: Option<PolicyRuleRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolApprovalResolved {
    pub call_id: String,
    pub approval_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub decision: ApprovalOutcome,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolLifecycle {
    pub call_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolCompleted {
    pub call_id: String,
    pub name: String,
    pub result: Value,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolFailed {
    pub call_id: String,
    pub name: String,
    pub error_code: String,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanCreated {
    pub plan_id: String,
    pub title: Option<String>,
    pub total_steps: u32,
    /// Full step list with statuses, so a client can render pending steps that
    /// never emit their own `plan.step_*` event.
    #[serde(default)]
    pub steps: Vec<PlanStep>,
    /// Durable `plans.id` the runtime persisted for this model plan, when the
    /// run's session is bound to a legacy conversation. Clients use it to
    /// approve/execute the plan through the App Protocol; `None` when the
    /// session is not conversation-bound.
    #[serde(default)]
    pub store_plan_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanStep {
    pub plan_id: String,
    pub position: u32,
    pub title: String,
    pub status: String,
    pub result_summary: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanProgressStatus {
    Executing,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlanProgress {
    pub plan_id: String,
    pub completed_steps: u32,
    pub total_steps: u32,
    pub message: Option<String>,
    pub status: PlanProgressStatus,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ArtifactCreated {
    pub artifact_id: String,
    pub kind: String,
    pub name: String,
    pub media_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct UsageUpdated {
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    #[ts(type = "number")]
    pub total_tokens: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BudgetEvent {
    pub window: String,
    pub spend_usd: f64,
    pub limit_usd: f64,
    pub percent: f64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentEvent {
    pub subagent_run_id: String,
    pub name: Option<String>,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SubagentProgress {
    pub subagent_run_id: String,
    pub message: String,
    pub percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorkerEvent {
    pub worker_id: String,
    pub attempt: u32,
    pub code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PluginStatusEvent {
    pub plugin_id: String,
    pub status: String,
    pub code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchStage {
    pub stage: String,
    pub message: Option<String>,
    pub progress: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchStarted {
    pub research_run_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchSource {
    pub url: String,
    pub title: Option<String>,
    pub snippet: Option<String>,
    /// Python stores confidence as a label (`high`/`medium`/`low`), not a
    /// numeric score.
    pub confidence: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchSubquestion {
    pub index: u32,
    pub question: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ResearchTerminal {
    pub artifact_id: Option<String>,
    pub source_count: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ClientState {
    pub run_status: Option<String>,
    pub content: String,
    pub reasoning: String,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    #[serde(default)]
    pub approvals: BTreeMap<String, String>,
    pub active_plan_id: Option<String>,
    pub plan_status: Option<String>,
    #[serde(default)]
    pub plan_steps: BTreeMap<String, String>,
    #[serde(default)]
    pub plan_completed_steps: u32,
    #[serde(default)]
    pub plan_total_steps: u32,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub subagents: BTreeMap<String, String>,
    #[serde(default)]
    pub workers: BTreeMap<String, String>,
    #[serde(default)]
    pub plugins: BTreeMap<String, String>,
    pub budget_status: Option<String>,
    pub research_status: Option<String>,
    #[ts(type = "number | null")]
    pub last_seq: Option<u64>,
    #[serde(skip)]
    #[ts(skip)]
    seen_events: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayError(pub String);

impl std::fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReplayError {}

impl ClientState {
    pub fn try_apply(&mut self, envelope: &EventEnvelope) -> Result<(), ReplayError> {
        let fingerprint = serde_json::to_string(envelope)
            .map_err(|error| ReplayError(format!("event serialization failed: {error}")))?;
        if let Some(previous) = self.seen_events.get(&envelope.event_id) {
            if previous == &fingerprint {
                return Ok(());
            }
            return Err(ReplayError(format!(
                "event id {} was reused with different content",
                envelope.event_id
            )));
        }
        let event_plan_id = match &envelope.event {
            CanonicalEvent::PlanCreated(payload) => Some(payload.plan_id.as_str()),
            CanonicalEvent::PlanStepStarted(payload) => Some(payload.plan_id.as_str()),
            CanonicalEvent::PlanStepCompleted(payload) => Some(payload.plan_id.as_str()),
            CanonicalEvent::PlanProgress(payload) => Some(payload.plan_id.as_str()),
            _ => None,
        };
        if let (Some(active), Some(incoming)) = (&self.active_plan_id, event_plan_id)
            && active != incoming
        {
            return Err(ReplayError(format!(
                "plan id mismatch: active {active}, got {incoming}"
            )));
        }
        if let Some(last_seq) = self.last_seq {
            if envelope.seq <= last_seq {
                return Err(ReplayError(format!(
                    "stale or conflicting sequence {} after {last_seq}",
                    envelope.seq
                )));
            }
            if envelope.seq != last_seq + 1 {
                return Err(ReplayError(format!(
                    "sequence gap: expected {}, got {}",
                    last_seq + 1,
                    envelope.seq
                )));
            }
        } else if envelope.seq != 1 {
            return Err(ReplayError(format!(
                "sequence must start at 1, got {}",
                envelope.seq
            )));
        }
        self.seen_events
            .insert(envelope.event_id.clone(), fingerprint);
        self.last_seq = Some(envelope.seq);
        match &envelope.event {
            CanonicalEvent::RunStarted(_) => self.run_status = Some("running".to_owned()),
            CanonicalEvent::RunCompleted(_) => self.run_status = Some("completed".to_owned()),
            CanonicalEvent::RunFailed(_) => self.run_status = Some("failed".to_owned()),
            CanonicalEvent::RunCancelled(_) => self.run_status = Some("cancelled".to_owned()),
            CanonicalEvent::ContentDelta(payload) => self.content.push_str(&payload.text),
            CanonicalEvent::ReasoningDelta(payload) => self.reasoning.push_str(&payload.text),
            CanonicalEvent::ToolRequested(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "requested".to_owned());
            }
            CanonicalEvent::ToolApprovalRequired(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "awaiting_approval".to_owned());
                self.approvals
                    .insert(payload.approval_id.clone(), "pending".to_owned());
            }
            CanonicalEvent::ToolApprovalResolved(payload) => {
                self.approvals.insert(
                    payload.approval_id.clone(),
                    match payload.decision {
                        ApprovalOutcome::Approved => "approved",
                        ApprovalOutcome::Denied => "denied",
                        ApprovalOutcome::TimedOut => "timed_out",
                    }
                    .to_owned(),
                );
            }
            CanonicalEvent::ToolStarted(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "running".to_owned());
            }
            CanonicalEvent::ToolCompleted(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "completed".to_owned());
            }
            CanonicalEvent::ToolFailed(payload) => {
                self.tools
                    .insert(payload.call_id.clone(), "failed".to_owned());
            }
            CanonicalEvent::PlanCreated(payload) => {
                self.active_plan_id = Some(payload.plan_id.clone());
                self.plan_status = Some("planned".to_owned());
                self.plan_completed_steps = 0;
                self.plan_total_steps = payload.total_steps;
                self.plan_steps.clear();
            }
            CanonicalEvent::PlanStepStarted(payload) => {
                self.active_plan_id
                    .get_or_insert_with(|| payload.plan_id.clone());
                self.plan_status = Some("running".to_owned());
                self.plan_steps
                    .insert(payload.position.to_string(), "running".to_owned());
            }
            CanonicalEvent::PlanStepCompleted(payload) => {
                self.active_plan_id
                    .get_or_insert_with(|| payload.plan_id.clone());
                self.plan_steps
                    .insert(payload.position.to_string(), payload.status.clone());
            }
            CanonicalEvent::PlanProgress(payload) => {
                self.active_plan_id
                    .get_or_insert_with(|| payload.plan_id.clone());
                self.plan_completed_steps = payload.completed_steps;
                self.plan_total_steps = payload.total_steps;
                self.plan_status = Some(
                    match payload.status {
                        PlanProgressStatus::Executing => "running",
                        PlanProgressStatus::Completed => "completed",
                        PlanProgressStatus::Failed => "failed",
                    }
                    .to_owned(),
                );
            }
            CanonicalEvent::ArtifactCreated(payload) => {
                if !self.artifacts.contains(&payload.artifact_id) {
                    self.artifacts.push(payload.artifact_id.clone());
                }
            }
            CanonicalEvent::BudgetWarning(_) => {
                self.budget_status = Some("warning".to_owned());
            }
            CanonicalEvent::BudgetExceeded(_) => {
                self.budget_status = Some("exceeded".to_owned());
            }
            CanonicalEvent::SubagentStarted(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "running".to_owned());
            }
            CanonicalEvent::SubagentProgress(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "running".to_owned());
            }
            CanonicalEvent::SubagentCompleted(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "completed".to_owned());
            }
            CanonicalEvent::SubagentFailed(payload) => {
                self.subagents
                    .insert(payload.subagent_run_id.clone(), "failed".to_owned());
            }
            CanonicalEvent::WorkerStarted(payload) => {
                self.workers
                    .insert(payload.worker_id.clone(), "running".to_owned());
            }
            CanonicalEvent::WorkerFailed(payload) => {
                self.workers
                    .insert(payload.worker_id.clone(), "failed".to_owned());
            }
            CanonicalEvent::WorkerRestarted(payload) => {
                self.workers
                    .insert(payload.worker_id.clone(), "running".to_owned());
            }
            CanonicalEvent::PluginStatus(payload) => {
                self.plugins
                    .insert(payload.plugin_id.clone(), payload.status.clone());
            }
            CanonicalEvent::ResearchStarted(_) | CanonicalEvent::ResearchStage(_) => {
                self.research_status = Some("running".to_owned());
            }
            CanonicalEvent::ResearchCompleted(_) => {
                self.research_status = Some("completed".to_owned());
            }
            CanonicalEvent::ResearchFailed(_) => {
                self.research_status = Some("failed".to_owned());
            }
            CanonicalEvent::ResearchCancelled(_) => {
                self.research_status = Some("cancelled".to_owned());
            }
            _ => {}
        }
        Ok(())
    }

    pub fn try_replay(events: &[EventEnvelope]) -> Result<Self, ReplayError> {
        let mut state = Self::default();
        let mut ordered = events.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|event| event.seq);
        let run_id = ordered.first().map(|event| event.run_id.as_str());
        for event in ordered {
            if Some(event.run_id.as_str()) != run_id {
                return Err(ReplayError("a client state cannot mix run ids".to_owned()));
            }
            state.try_apply(event)?;
        }
        Ok(state)
    }

    pub fn replay(events: &[EventEnvelope]) -> Self {
        Self::try_replay(events).expect("valid canonical event sequence")
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct GoldenTrace {
    pub name: String,
    pub events: Vec<EventEnvelope>,
    pub expected_state: ClientState,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export)]
pub enum StreamFrame {
    Event(Box<EventEnvelope>),
    Keepalive(StreamKeepalive),
    End(StreamEnd),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum RpcId {
    String(#[schemars(length(max = 128))] String),
    Integer(#[ts(type = "number")] i64),
    Null,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcRequest {
    pub jsonrpc: JsonRpcV2,
    pub id: RpcId,
    pub method: CoolCommandMethod,
    pub params: CommandEnvelope,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TransportLimits {
    pub max_frame_bytes: u32,
    pub max_rpc_id_bytes: u16,
    pub max_in_flight: u16,
    pub outbound_queue: u16,
    pub event_page_limit: u16,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct InitializeResult {
    #[ts(type = "1")]
    pub protocol_version: V1Version,
    pub server_name: String,
    pub server_version: String,
    pub capabilities: BTreeSet<String>,
    pub limits: TransportLimits,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionCreatedResult {
    pub session_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionLoadedResult {
    pub session_id: String,
    pub active_run_id: Option<String>,
    #[ts(type = "number | null")]
    pub last_seq: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PromptAcceptedResult {
    pub run_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunCancelledResult {
    pub run_id: String,
    pub accepted: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RunSubscribedResult {
    pub run_id: String,
    pub session_id: String,
    /// Durable cursor to catch up from; live events continue with `seq > last_seq`.
    #[ts(type = "number")]
    pub last_seq: u64,
    /// True when the run is already terminal; no live events will follow.
    pub terminal: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ApprovalResolvedResult {
    pub approval_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub outcome: ApprovalOutcome,
    /// The persisted rule when the resolve carried `remember` (P1.6).
    #[serde(default)]
    pub remembered_rule: Option<PolicyRuleRecord>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionSummary {
    pub session_id: String,
    pub title: Option<String>,
    pub project_key: Option<String>,
    pub active_run_id: Option<String>,
    #[ts(type = "number | null")]
    pub last_seq: Option<u64>,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionListResult {
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct HistoryItem {
    /// Durable cursor of the item's own event (the `rust_events.rowid`), stable
    /// for the lifetime of the store. Clients can use it as a render key.
    pub cursor: u64,
    /// RFC3339 timestamp of the item's event.
    pub occurred_at: String,
    /// Durable run the item belongs to.
    pub run_id: String,
    pub role: String,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolRequested>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    /// Model that produced the assistant item, when the run recorded one.
    pub model: Option<String>,
    /// Cumulative usage reported by the model call that produced the assistant
    /// item (absent for user/tool items and imported history).
    pub usage: Option<UsageUpdated>,
    /// Set only on a `role: "summary"` item: the `session.compacted` cutoff.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub compact_up_to_cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionHistoryResult {
    #[serde(default)]
    pub items: Vec<HistoryItem>,
    pub has_more: bool,
    /// Exclusive cursor for the next older page, or `None` when this page is
    /// the oldest. Pass it as `beforeCursor` to continue paging.
    #[ts(type = "number | null")]
    pub next_cursor: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionForkedResult {
    pub session_id: String,
    pub forked_from: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionRewindResult {
    pub session_id: String,
    /// The seed run carrying the retained history (mode `rewind`).
    pub run_id: String,
    /// Runs superseded by the rewind (now `rewound` status).
    #[serde(default)]
    pub rewound_run_ids: Vec<String>,
    #[ts(type = "number")]
    pub to_cursor: u64,
    /// Checkpoint ref the workspace was restored to, when one applied.
    #[serde(default)]
    pub checkpoint_ref: Option<String>,
    #[serde(default)]
    pub workspace_restored: bool,
    /// Why workspace restore did not apply (no checkpoint, not a restorable
    /// snapshot, launcher disabled, restore failed). Present only when
    /// `restoreWorkspace` was requested and `workspace_restored` is false.
    #[serde(default)]
    pub restore_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionConversationResult {
    pub session_id: String,
    pub conversation_id: i64,
    pub created: bool,
    /// Canonical history events projected from the legacy transcript.
    #[ts(type = "number")]
    pub imported_events: u64,
    /// True when the projection is not the complete legacy transcript: the
    /// bound was reached and/or leading orphan tool rows were dropped. Older
    /// messages stay readable through `conversations.messages`.
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionRunSummary {
    pub run_id: String,
    pub status: String,
    #[ts(type = "number")]
    pub last_seq: u64,
    pub finish_reason: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SessionRunsResult {
    #[serde(default)]
    pub runs: Vec<SessionRunSummary>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SteerAcceptedResult {
    pub run_id: String,
    #[ts(type = "number")]
    pub seq: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StatusEntry {
    pub id: String,
    pub status: String,
    pub code: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StatusGetResult {
    #[serde(default)]
    pub plugins: Vec<StatusEntry>,
    #[serde(default)]
    pub workers: Vec<StatusEntry>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

/// One tool registered in the running Rust agent runtime, as shown by the
/// agent-constructor and subagent tool pickers. `dangerous` marks a tool whose
/// default decision is `Ask` (requires approval); macro-backed tools are
/// flagged via the `macro_` name prefix.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ToolCatalogRecord {
    pub name: String,
    pub description: String,
    pub dangerous: bool,
    pub capabilities: Vec<String>,
    pub parameters: Value,
    pub is_macro: bool,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export)]
pub enum ResponsePayload {
    Initialized(InitializeResult),
    SessionCreated(SessionCreatedResult),
    SessionLoaded(SessionLoadedResult),
    SessionListed(SessionListResult),
    SessionHistory(SessionHistoryResult),
    SessionForked(SessionForkedResult),
    SessionRewound(SessionRewindResult),
    SessionForConversation(SessionConversationResult),
    SessionRuns(SessionRunsResult),
    PromptAccepted(PromptAcceptedResult),
    SteerAccepted(SteerAcceptedResult),
    RunCancelled(RunCancelledResult),
    RunSubscribed(RunSubscribedResult),
    ApprovalResolved(ApprovalResolvedResult),
    PolicyRulesListed(PolicyRulesListResult),
    PolicyRuleAdded(PolicyRuleRecord),
    PolicyRuleDeleted(PolicyRuleDeleteResult),
    EventPage(EventPage),
    Status(StatusGetResult),
    ToolsListed(Vec<ToolCatalogRecord>),
    ExtensionsStatus(ExtensionStatusResult),
    ExtensionsPluginEnabled(PluginRecord),
    ExtensionsHookReviewed(HookRecord),
    SettingsSystemPrompt(SystemPromptRecord),
    McpServersListed(McpServerListResult),
    McpServerAdded(McpServerAdminRecord),
    McpServerUpdated(McpServerAdminRecord),
    McpServerRemoved(LegacyOkResult),
    McpConnected(McpConnectResult),
    McpDisconnected(McpConnectResult),
    McpHealth(McpHealthResult),
    McpToolsListed(McpToolListResult),
    McpReconnected(McpServerListResult),
    McpStoreSearched(McpStoreSearchResult),
    McpStoreInstalled(McpConnectResult),
    SkillsListed(SkillListResult),
    SkillCreated(SkillCreateResult),
    SkillDeleted(LegacyOkResult),
    ConversationsList(Vec<ConversationRecord>),
    ConversationsCreated(ConversationRecord),
    ConversationsGot(ConversationRecord),
    ConversationsMessages(Vec<MessageRecord>),
    ConversationsUpdated(ConversationRecord),
    ConversationsDeleted(DeletedResult),
    ConversationsCompacted(CompactResult),
    ConversationsApprovals(Vec<ApprovalAuditRecord>),
    ConversationsSearched(Vec<ConversationRecord>),
    ConversationsBulk(AffectedResult),
    RunsListed(Vec<AgentRunRecord>),
    RunsGot(AgentRunRecord),
    RunsEvents(Vec<RunEventRecord>),
    RunsCancelled(AgentRunRecord),
    ProvidersListed(Vec<ProviderRecord>),
    ProvidersCreated(ProviderRecord),
    ProvidersGot(ProviderRecord),
    ProvidersUpdated(ProviderRecord),
    ProvidersDeleted(DeletedResult),
    ProvidersModels(Vec<ModelInfoRecord>),
    ProvidersModelsLive(Vec<ModelInfoRecord>),
    ProvidersModelsPreview(Vec<ModelInfoRecord>),
    ProvidersOauthStarted(ProvidersOauthStartResult),
    ProvidersOauthCompleted(ProvidersOauthCompleteResult),
    MemoryListed(Vec<MemoryRecord>),
    MemoryGot(MemoryRecord),
    MemoryCreated(MemoryRecord),
    MemoryUpdated(MemoryRecord),
    MemoryDeleted(LegacyOkResult),
    MemoryPending(Vec<MemoryRecord>),
    MemoryConfirmed(MemoryRecord),
    MemoryRejected(LegacyOkResult),
    MemoryPinned(MemoryRecord),
    MemoryExplained(MemoryExplainRecord),
    MemoryExtracted(MemoryExtractResult),
    MemoryEpisodes(Vec<EpisodeRecord>),
    MemoryStats(MemoryStatsRecord),
    EntitiesListed(Vec<EntityRecord>),
    EntitiesGot(EntityRecord),
    EntitiesCreated(EntityRecord),
    EntitiesUpdated(EntityRecord),
    EntitiesDeleted(LegacyOkResult),
    PlansListed(Vec<PlanRecord>),
    PlansGot(PlanRecord),
    PlansUpdated(PlanRecord),
    PlansApproved(PlanRecord),
    PlansExecuted(PlanExecuteResult),
    PlansCancelled(PlanRecord),
    PlansTemplatesListed(Vec<PlanTemplateRecord>),
    PlansTemplatesCreated(PlanTemplateRecord),
    PlansTemplatesDeleted(DeletedResult),
    SubagentsRolesListed(Vec<SubagentRoleRecord>),
    SubagentsRolesGot(SubagentRoleRecord),
    SubagentsRolesCreated(SubagentRoleRecord),
    SubagentsRolesUpdated(SubagentRoleRecord),
    SubagentsRolesDeleted(DeletedResult),
    SubagentsLaunched(SubagentRunRecord),
    SubagentsLaunchedBatch(Vec<SubagentRunRecord>),
    SubagentsRunsListed(Vec<SubagentRunRecord>),
    SubagentsRunsGot(SubagentRunDetailRecord),
    SubagentsRunsCancelled(SubagentRunCancelResult),
    SubagentsRunsDeleted(LegacyOkResult),
    InspectorTimeline(TimelineRecord),
    InspectorCompared(RunComparisonRecord),
    InspectorReplayed(ReplayResult),
    BudgetsGot(BudgetStatusRecord),
    BudgetsUpdated(BudgetStatusRecord),
    BudgetsOverrideSet(BudgetStatusRecord),
    BudgetsOverrideCleared(BudgetStatusRecord),
    BudgetsSpend(Vec<SpendEntryRecord>),
    ArtifactsListed(Vec<ArtifactRecord>),
    ArtifactsGot(ArtifactDetailRecord),
    ArtifactsDeleted(DeletedResult),
    WorkspaceGitInfo(GitInfoRecord),
    WorkspaceDirectories(DirectoryListingRecord),
    WorkspaceRecent(RecentDirectoriesRecord),
    WorkspaceGitStatus(GitStatusRecord),
    WorkspaceGitLog(GitLogRecord),
    WorkspaceGitBranches(GitBranchesRecord),
    WorkspaceGitCheckout(GitCheckoutResult),
    ProfilesListed(Vec<ProfileRecord>),
    ProfilesGot(ProfileRecord),
    ProfilesCreated(ProfileRecord),
    ProfilesUpdated(ProfileRecord),
    ProfilesDeleted(DeletedResult),
    ProfilesSeeded(SeedResult),
    ProfilesCloned(ProfileRecord),
    ProfilesPlayground(PlaygroundResult),
    AnalyticsSummary(AnalyticsSummaryRecord),
    AnalyticsSpendOverTime(Vec<SpendBucketRecord>),
    AnalyticsSpendByModel(Vec<ModelSpendRecord>),
    AnalyticsTopTools(Vec<ToolUsageRecord>),
    AnalyticsLatency(Vec<LatencyBucketRecord>),
    AnalyticsCallHistory(CallHistoryResult),
    AnalyticsMemoryActivity(Vec<MemoryActivityBucketRecord>),
    TasksListed(Vec<TaskRecord>),
    TasksGot(TaskRecord),
    TasksCreated(TaskRecord),
    TasksUpdated(TaskRecord),
    TasksDeleted(DeletedResult),
    TasksRan(TaskRunRecord),
    TasksRunsListed(Vec<TaskRunRecord>),
    TasksRunsGot(TaskRunDetailRecord),
    TasksRunsCancelled(TaskRunCancelResult),
    TasksRunsRead(TaskRunRecord),
    TasksInbox(TaskInboxResult),
    TasksScheduler(SchedulerStatusRecord),
    TasksTemplatesListed(Vec<TaskTemplateRecord>),
    TasksParsedCron(ParseCronResult),
    RssSubscriptionsListed(Vec<RssSubscriptionRecord>),
    RssSubscribed(RssSubscriptionRecord),
    RssUnsubscribed(DeletedResult),
    RssEntriesListed(Vec<RssEntryRecord>),
    RssEntriesAll(Vec<RssEntryRecord>),
    RssEntryRead(RssEntryRecord),
    RssFetched(RssFetchResult),
    WebhooksListed(Vec<WebhookEndpointRecord>),
    WebhooksGot(WebhookEndpointRecord),
    WebhooksCreated(WebhookEndpointRecord),
    WebhooksUpdated(WebhookEndpointRecord),
    WebhooksDeleted(DeletedResult),
    WebhooksEvents(Vec<WebhookEventRecord>),
    WebhooksReplayed(WebhookEventRecord),
    WikiListed(Vec<WikiArticleRecord>),
    WikiSearched(Vec<WikiArticleRecord>),
    WikiGot(WikiArticleRecord),
    WikiCreated(WikiArticleRecord),
    WikiUpdated(WikiArticleRecord),
    WikiDeleted(DeletedResult),
    WikiCategories(Vec<String>),
    WikiStats(WikiStatsRecord),
    WikiPromoted(WikiArticleRecord),
    ResearchListed(Vec<ResearchRunRecord>),
    ResearchGot(ResearchRunDetailRecord),
    ResearchCreated(ResearchRunRecord),
    ResearchCancelled(ResearchCancelResult),
    ResearchReran(ResearchRunRecord),
    ConstructorMacros(Vec<MacroToolRecord>),
    ConstructorMacrosCreated(MacroToolRecord),
    ConstructorMacrosUpdated(MacroToolRecord),
    ConstructorMacrosDeleted(DeletedResult),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcSuccess {
    pub jsonrpc: JsonRpcV2,
    pub id: RpcId,
    pub result: ResponsePayload,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcFailure {
    pub jsonrpc: JsonRpcV2,
    pub id: RpcId,
    pub error: ProtocolError,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct RpcNotification {
    pub jsonrpc: JsonRpcV2,
    pub method: RunEventMethod,
    pub params: StreamFrame,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum ServerFrame {
    Success(RpcSuccess),
    Failure(RpcFailure),
    Notification(RpcNotification),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StreamKeepalive {
    pub run_id: String,
    #[ts(type = "number")]
    pub last_seq: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct StreamEnd {
    pub run_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolSchemaDocument {
    pub command: CommandEnvelope,
    pub rpc_request: RpcRequest,
    pub server_frame: ServerFrame,
    pub event: EventEnvelope,
    pub event_page: EventPage,
    pub error: ProtocolError,
    pub golden_trace: GoldenTrace,
    pub stream_frame: StreamFrame,
}
