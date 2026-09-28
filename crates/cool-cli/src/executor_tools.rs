//! Executor-bound agent tools (M12): `deep_research`, `spawn_subagent`, and
//! the skill trio. Unlike the store tools these need server-owned handles —
//! the subagent/research executors and the skills admin — so they are built
//! after `build_server` and registered onto the shared [`ToolRegistry`]
//! (clones share the backing map, so the live agent runtime sees them
//! immediately).

use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{MAX_SPAWN_DEPTH, Tool, ToolContext, ToolError, ToolHandler, ToolResult};
use cool_app_server::{
    ForkContext, ResearchExecutor, SkillAdmin, SubagentExecutor, SubagentIsolation,
    SubagentLaunchSpec,
};
use cool_protocol::SkillCreateParams;
use cool_security::{Capability, Decision};
use cool_store::LegacyStore;
use cool_store::domains::conversations::NewMessage;
use cool_store::domains::subagents::SubagentRunFilter;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::store_tools::{
    definition, optional_i64, optional_string, optional_string_array, reject_unknown,
    required_string,
};

/// Registers the executor-bound tools. `None` executors yield tools that
/// report `executor_unavailable` rather than dropping the tool, so a store-less
/// boot still advertises the catalog.
pub fn executor_tool_registry(
    store: Arc<LegacyStore>,
    subagents: Option<Arc<SubagentExecutor>>,
    research: Option<Arc<ResearchExecutor>>,
    skills: Option<Arc<dyn SkillAdmin>>,
) -> Vec<Tool> {
    vec![
        Tool::new(
            definition(
                "deep_research",
                "Run a deep web research on a topic: decomposes it into sub-questions, gathers web sources in parallel, and returns a cited markdown report. Expensive — use only when the user asks for in-depth research.",
                json!({"type":"object","properties":{"topic":{"type":"string","description":"Research topic or question"},"depth":{"type":"integer","minimum":3,"maximum":5,"default":4,"description":"Research depth 3-5 (breadth vs depth tradeoff)"},"model":{"type":"string","description":"Optional model override"}},"required":["topic"],"additionalProperties":false}),
            ),
            [Capability::Network, Capability::Execute],
            Decision::Ask,
            DeepResearch { research },
        )
        // Rare, expensive tool — stays hidden until search/activate (P1.10).
        .deferred(),
        Tool::new(
            definition(
                "spawn_subagent",
                "Spawn a subagent to handle a sub-task and return its result. Optionally bind a role name or agent profile slug. With background=true the call returns immediately; poll or steer it via collect_subagent / send_to_subagent / list_subagents.",
                json!({"type":"object","properties":{"prompt":{"type":"string","description":"Task for the subagent"},"role":{"type":"string","description":"Subagent role name"},"profile":{"type":"string","description":"Agent profile slug (takes precedence over role)"},"model":{"type":"string","description":"Optional model override"},"background":{"type":"boolean","default":false,"description":"Return immediately instead of blocking until the subagent finishes"},"fork_context":{"type":"string","enum":["none","summary","full"],"default":"none","description":"Parent context passed to the child: none, a summarised digest, or the full transcript"},"isolation":{"type":"string","enum":["shared","worktree"],"default":"shared","description":"shared = same workspace; worktree = isolated git worktree under .cool/worktrees"}},"required":["prompt"],"additionalProperties":false}),
            ),
            [Capability::Execute],
            Decision::Ask,
            SpawnSubagent {
                store: store.clone(),
                subagents: subagents.clone(),
            },
        ),
        Tool::new(
            definition(
                "collect_subagent",
                "Collect the status (and result, when finished) of a subagent run started by spawn_subagent. wait=true blocks until it reaches a terminal state or timeoutSecs elapses.",
                json!({"type":"object","properties":{"subagentRunId":{"type":"integer","description":"subagent_runs id returned by spawn_subagent"},"wait":{"type":"boolean","default":true,"description":"Block until the run is terminal (or times out)"},"timeoutSecs":{"type":"number","default":300,"description":"Max seconds to wait when wait=true"}},"required":["subagentRunId"],"additionalProperties":false}),
            ),
            [Capability::Execute],
            Decision::Allow,
            CollectSubagent {
                subagents: subagents.clone(),
            },
        ),
        Tool::new(
            definition(
                "send_to_subagent",
                "Send a follow-up user message to a running subagent — delivered as a steer on the child's next iteration. Use after spawn_subagent(background=true).",
                json!({"type":"object","properties":{"subagentRunId":{"type":"integer","description":"subagent_runs id returned by spawn_subagent"},"message":{"type":"string","description":"Steer message delivered to the child run"}},"required":["subagentRunId","message"],"additionalProperties":false}),
            ),
            [Capability::Execute],
            Decision::Ask,
            SendToSubagent {
                store: store.clone(),
                subagents: subagents.clone(),
            },
        ),
        Tool::new(
            definition(
                "list_subagents",
                "List subagent runs spawned from this conversation, newest first.",
                json!({"type":"object","properties":{"status":{"type":"string","description":"Optional status filter: queued|running|completed|failed|cancelled"},"limit":{"type":"integer","default":20,"description":"Max runs to return"}},"additionalProperties":false}),
            ),
            [Capability::Execute],
            Decision::Allow,
            ListSubagents { store },
        ),
        Tool::new(
            definition(
                "list_skills",
                "List available skills. Skills are reusable AI capability modules that provide specialized instructions for tasks like research, coding, summarization, translation, and brainstorming.",
                json!({"type":"object","properties":{"source":{"type":"string","description":"Filter by source: builtin, user, or plugin"}},"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            ListSkills {
                skills: skills.clone(),
            },
        ),
        Tool::new(
            definition(
                "use_skill",
                "Activate a skill by name to get specialized instructions for a task. The skill's instructions will guide your approach. Use list_skills first to see what's available.",
                json!({"type":"object","properties":{"name":{"type":"string","description":"Name of the skill to activate"}},"required":["name"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            UseSkill {
                skills: skills.clone(),
            },
        ),
        Tool::new(
            definition(
                "create_skill",
                "Create a new skill with a name, description, tags, and instruction body. Skills are stored as SKILL.md files and become immediately available. Use scope='global' for shared skills or scope='user' for personal ones.",
                json!({"type":"object","properties":{"name":{"type":"string","description":"Skill name: lowercase alphanumeric with hyphens (e.g. 'my-skill')"},"description":{"type":"string","default":"","description":"Short description of what the skill does"},"tags":{"type":"array","items":{"type":"string"},"default":[],"description":"Keywords for relevance matching"},"tools":{"type":"array","items":{"type":"string"},"default":[],"description":"Recommended tools for the skill"},"body":{"type":"string","description":"Skill instruction content (markdown)"},"scope":{"type":"string","default":"user","description":"global or user"}},"required":["name","body"],"additionalProperties":false}),
            ),
            [],
            Decision::Allow,
            CreateSkill { skills },
        )
        // Rare tool — stays hidden until search/activate (P1.10).
        .deferred(),
    ]
}

struct DeepResearch {
    research: Option<Arc<ResearchExecutor>>,
}

#[async_trait]
impl ToolHandler for DeepResearch {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["topic", "depth", "model"])?;
        let Some(executor) = &self.research else {
            return Ok(ToolResult::error(
                "executor_unavailable",
                "research executor is not available on this server",
            ));
        };
        let topic = required_string(&arguments, "topic")?;
        let depth = optional_i64(&arguments, "depth")?.unwrap_or(4).clamp(3, 5);
        let model = optional_string(&arguments, "model")?;
        let cancel = context.cancel.as_ref().map(|signal| signal.receiver());
        let outcome = executor
            .run_inline(topic, depth, model, context.conversation_id, None, cancel)
            .await
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        Ok(match outcome.status {
            "completed" => ToolResult::ok(json!({
                "report": outcome.report.unwrap_or_default(),
                "researchRunId": outcome.run_id,
                "reportArtifactId": outcome.report_artifact_id,
                "sourceCount": outcome.source_count,
            })),
            "cancelled" => ToolResult::error("cancelled", "research was cancelled"),
            _ => ToolResult::error(
                "research_failed",
                outcome
                    .error
                    .unwrap_or_else(|| "research pipeline failed".to_owned()),
            ),
        }
        .masked())
    }
}

struct SpawnSubagent {
    store: Arc<LegacyStore>,
    subagents: Option<Arc<SubagentExecutor>>,
}

#[async_trait]
impl ToolHandler for SpawnSubagent {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &[
                "prompt",
                "role",
                "profile",
                "model",
                "background",
                "fork_context",
                "isolation",
            ],
        )?;
        let Some(executor) = &self.subagents else {
            return Ok(ToolResult::error(
                "executor_unavailable",
                "subagent executor is not available on this server",
            ));
        };
        // P1.7 depth limit: a run at MAX_SPAWN_DEPTH cannot spawn children.
        if context.spawn_depth >= MAX_SPAWN_DEPTH {
            return Ok(ToolResult::error(
                "spawn_depth_exceeded",
                format!(
                    "spawn_subagent is unavailable at depth {MAX_SPAWN_DEPTH}; run the step directly"
                ),
            ));
        }
        let prompt = required_string(&arguments, "prompt")?.to_owned();
        let role = optional_string(&arguments, "role")?;
        let profile = optional_string(&arguments, "profile")?;
        let model = optional_string(&arguments, "model")?;
        let background = arguments
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let fork_context =
            ForkContext::parse(optional_string(&arguments, "fork_context")?.as_deref())
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let isolation =
            SubagentIsolation::parse(optional_string(&arguments, "isolation")?.as_deref())
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;

        // Python `_spawn_subagent` precedence: profile slug wins; a role is
        // resolved only when no profile was given.
        let mut profile_id = None;
        if let Some(slug) = &profile {
            let profiles = self
                .store
                .list_profiles(false)
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
            match profiles.iter().find(|entry| entry.slug == *slug) {
                Some(entry) => profile_id = Some(entry.id),
                None => {
                    return Ok(ToolResult::error(
                        "unknown_profile",
                        format!(
                            "Unknown agent profile: '{slug}'. Available profiles can be listed via the API."
                        ),
                    ));
                }
            }
        }
        let mut role_id = None;
        if let Some(name) = &role
            && profile_id.is_none()
        {
            let roles = self
                .store
                .list_subagent_roles()
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
            match roles.iter().find(|entry| &entry.name == name) {
                Some(entry) => role_id = Some(entry.id),
                None => {
                    return Ok(ToolResult::error(
                        "unknown_role",
                        format!(
                            "Unknown subagent role: '{name}'. Available roles can be listed via the API."
                        ),
                    ));
                }
            }
        }

        let spec = SubagentLaunchSpec {
            parent_conversation_id: context.conversation_id.unwrap_or(1),
            role_id,
            profile_id,
            // No legacy agent_runs id exists on the canonical path; attribution
            // flows through the parent conversation.
            parent_run_id: None,
            research_run_id: None,
            name: None,
            prompt: prompt.clone(),
            model,
            fork_context,
            isolation,
            spawn_depth: context.spawn_depth,
            parent_history: context.history_snapshot.clone().unwrap_or_default(),
        };
        // Python parity: a fresh key per call — a subagent launch must never
        // dedupe against an earlier spawn.
        let key = format!("spawn-subagent:{}", Uuid::new_v4());
        let run = executor
            .launch(&context.actor_id, spec, &key, &key)
            .await
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;

        // background=true (P1.7): hand the run id back immediately; the parent
        // collects/steers it via collect_subagent / send_to_subagent.
        if background {
            return Ok(ToolResult::ok(json!({
                "subagentRunId": run.id,
                "status": run.status,
                "background": true,
            }))
            .masked());
        }

        let mut cancel_rx = match &context.cancel {
            Some(signal) => signal.receiver(),
            None => {
                let (_tx, rx) = tokio::sync::watch::channel(None);
                rx
            }
        };
        let row = executor
            .await_terminal(&context.actor_id, run.id, &mut cancel_rx)
            .await
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        Ok(match row.status.as_str() {
            "completed" => ToolResult::ok(json!({
                "result": row.result_summary.unwrap_or_default(),
                "subagentRunId": row.id,
            })),
            "cancelled" => ToolResult::error("cancelled", "Subagent was cancelled."),
            _ => ToolResult::error(
                "subagent_failed",
                row.error
                    .unwrap_or_else(|| format!("Subagent failed: {}", row.status)),
            ),
        }
        .masked())
    }
}

struct ListSkills {
    skills: Option<Arc<dyn SkillAdmin>>,
}

#[async_trait]
impl ToolHandler for ListSkills {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["source"])?;
        let source = optional_string(&arguments, "source")?.filter(|value| !value.is_empty());
        let Some(admin) = &self.skills else {
            return Ok(ToolResult::error(
                "skills_unavailable",
                "skills store is not available on this server",
            ));
        };
        let list = admin
            .list(source.as_deref())
            .await
            .map_err(ToolError::InvalidArguments)?;
        if list.skills.is_empty() {
            return Ok(ToolResult::ok(json!(format!(
                "No skills available.{}",
                source
                    .map(|source| format!(" (source={source})"))
                    .unwrap_or_default()
            ))));
        }
        let mut text = format!("Available skills ({}):", list.skills.len());
        for skill in &list.skills {
            let tags = if skill.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", skill.tags.join(", "))
            };
            text.push_str(&format!(
                "\n- **{}** ({}): {}{tags}",
                skill.name, skill.source, skill.description
            ));
        }
        Ok(ToolResult::ok(json!(text)))
    }
}

struct UseSkill {
    skills: Option<Arc<dyn SkillAdmin>>,
}

#[async_trait]
impl ToolHandler for UseSkill {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["name"])?;
        let name = required_string(&arguments, "name")?;
        let Some(admin) = &self.skills else {
            return Ok(ToolResult::error(
                "skills_unavailable",
                "skills store is not available on this server",
            ));
        };
        let list = admin
            .list(None)
            .await
            .map_err(ToolError::InvalidArguments)?;
        let Some(skill) = list.skills.iter().find(|skill| skill.name == name) else {
            let available: Vec<&str> = list
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect();
            let hint = if available.is_empty() {
                String::new()
            } else {
                format!(" Available skills: {}", available.join(", "))
            };
            return Ok(ToolResult::error(
                "not_found",
                format!("Skill '{name}' not found.{hint}"),
            ));
        };
        let mut text = skill.body.clone();
        if !skill.tools.is_empty() {
            text.push_str(&format!(
                "\n## Recommended tools: {}",
                skill.tools.join(", ")
            ));
        }
        Ok(ToolResult::ok(json!(text)))
    }
}

struct CreateSkill {
    skills: Option<Arc<dyn SkillAdmin>>,
}

#[async_trait]
impl ToolHandler for CreateSkill {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &["name", "description", "tags", "tools", "body", "scope"],
        )?;
        let Some(admin) = &self.skills else {
            return Ok(ToolResult::error(
                "skills_unavailable",
                "skills store is not available on this server",
            ));
        };
        let params = SkillCreateParams {
            idempotency_key: cool_protocol::IdempotencyKey::new(format!(
                "tool-create-skill:{}",
                Uuid::new_v4()
            ))
            .map_err(|error| ToolError::InvalidArguments(error.to_owned()))?,
            name: required_string(&arguments, "name")?.to_owned(),
            description: optional_string(&arguments, "description")?.unwrap_or_default(),
            tags: optional_string_array(&arguments, "tags")?.unwrap_or_default(),
            tools: optional_string_array(&arguments, "tools")?.unwrap_or_default(),
            body: required_string(&arguments, "body")?.to_owned(),
            scope: optional_string(&arguments, "scope")?.unwrap_or_else(|| "user".to_owned()),
        };
        let result = admin
            .create(&context.actor_id, &params)
            .await
            .map_err(ToolError::InvalidArguments)?;
        Ok(ToolResult::ok(json!({
            "name": result.name,
            "path": result.path,
            "scope": result.scope,
        })))
    }
}

struct CollectSubagent {
    subagents: Option<Arc<SubagentExecutor>>,
}

#[async_trait]
impl ToolHandler for CollectSubagent {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["subagentRunId", "wait", "timeoutSecs"])?;
        let Some(executor) = &self.subagents else {
            return Ok(ToolResult::error(
                "executor_unavailable",
                "subagent executor is not available on this server",
            ));
        };
        let run_id = optional_i64(&arguments, "subagentRunId")?
            .ok_or_else(|| ToolError::InvalidArguments("missing subagentRunId".to_owned()))?;
        let wait = arguments
            .get("wait")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let timeout_secs = arguments
            .get("timeoutSecs")
            .and_then(Value::as_f64)
            .unwrap_or(300.0)
            .clamp(1.0, 3600.0);

        let mut cancel_rx = match &context.cancel {
            Some(signal) => signal.receiver(),
            None => {
                let (_tx, rx) = tokio::sync::watch::channel(None);
                rx
            }
        };
        let row = if wait {
            match tokio::time::timeout(
                std::time::Duration::from_secs_f64(timeout_secs),
                executor.await_terminal(&context.actor_id, run_id, &mut cancel_rx),
            )
            .await
            {
                Ok(Ok(row)) => row,
                Ok(Err(error)) => {
                    return Ok(ToolResult::error("unknown_run", error.to_string()));
                }
                Err(_) => {
                    // Timed out while still running — report the current row.
                    let row = executor
                        .get_run(&context.actor_id, run_id)
                        .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
                    return Ok(ToolResult::ok(json!({
                        "subagentRunId": row.id,
                        "status": row.status,
                        "timedOut": true,
                    }))
                    .masked());
                }
            }
        } else {
            executor
                .get_run(&context.actor_id, run_id)
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?
        };
        Ok(match row.status.as_str() {
            "completed" => ToolResult::ok(json!({
                "subagentRunId": row.id,
                "status": "completed",
                "result": row.result_summary.unwrap_or_default(),
            })),
            "queued" | "running" => ToolResult::ok(json!({
                "subagentRunId": row.id,
                "status": row.status,
            })),
            "cancelled" => ToolResult::error("cancelled", "Subagent was cancelled."),
            _ => ToolResult::error(
                "subagent_failed",
                row.error
                    .unwrap_or_else(|| format!("Subagent failed: {}", row.status)),
            ),
        }
        .masked())
    }
}

struct SendToSubagent {
    store: Arc<LegacyStore>,
    subagents: Option<Arc<SubagentExecutor>>,
}

#[async_trait]
impl ToolHandler for SendToSubagent {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["subagentRunId", "message"])?;
        let Some(executor) = &self.subagents else {
            return Ok(ToolResult::error(
                "executor_unavailable",
                "subagent executor is not available on this server",
            ));
        };
        let run_id = optional_i64(&arguments, "subagentRunId")?
            .ok_or_else(|| ToolError::InvalidArguments("missing subagentRunId".to_owned()))?;
        let message = required_string(&arguments, "message")?;
        if message.trim().is_empty() {
            return Ok(ToolResult::error(
                "invalid_arguments",
                "message must not be empty",
            ));
        }
        let run = executor
            .get_run(&context.actor_id, run_id)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        if !matches!(run.status.as_str(), "queued" | "running") {
            return Ok(ToolResult::error(
                "subagent_finished",
                format!(
                    "subagent run {} already reached a terminal state ({})",
                    run.id, run.status
                ),
            ));
        }
        // The steer path: the child's LegacyTranscriptSink drains user-role
        // messages past its cursor into the run's history each iteration —
        // the same mechanism operator steers use for top-level runs.
        self.store
            .add_message(
                &context.actor_id,
                run.conversation_id,
                &NewMessage {
                    role: "user".to_owned(),
                    content: Some(message.to_owned()),
                    ..NewMessage::default()
                },
            )
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        Ok(ToolResult::ok(json!({
            "subagentRunId": run.id,
            "status": run.status,
            "delivered": "steer",
        }))
        .masked())
    }
}

struct ListSubagents {
    store: Arc<LegacyStore>,
}

#[async_trait]
impl ToolHandler for ListSubagents {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["status", "limit"])?;
        let status = optional_string(&arguments, "status")?;
        let limit = optional_i64(&arguments, "limit")?
            .unwrap_or(20)
            .clamp(1, 200) as usize;
        let Some(parent_conversation_id) = context.conversation_id else {
            return Ok(ToolResult::ok(json!({
                "runs": [],
                "note": "no conversation on this run context",
            })));
        };
        let runs = self
            .store
            .list_subagent_runs(
                &context.actor_id,
                &SubagentRunFilter {
                    parent_conversation_id: Some(parent_conversation_id),
                    status,
                    research_run_id: None,
                    limit: Some(limit),
                },
            )
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let entries: Vec<Value> = runs
            .iter()
            .map(|run| {
                json!({
                    "subagentRunId": run.id,
                    "name": run.name,
                    "status": run.status,
                    "conversationId": run.conversation_id,
                    "result": run.result_summary,
                    "error": run.error,
                })
            })
            .collect();
        Ok(ToolResult::ok(json!({ "runs": entries })).masked())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cool_agent::{AgentRuntime, HostContext, ScriptedDriver, builtin_registry};
    use cool_security::{CapabilityPolicy, Decision, Workspace};
    use cool_state::DurableStore;
    use cool_store::domains::conversations::NewConversation;

    use super::*;

    struct Fixture {
        store: Arc<LegacyStore>,
        subagents: Arc<SubagentExecutor>,
        context: ToolContext,
        parent_conversation: i64,
        _directory: tempfile::TempDir,
    }

    fn fixture(driver: Arc<ScriptedDriver>) -> Fixture {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(LegacyStore::in_memory().expect("store"));
        store.ensure_actor("local-user").expect("actor");
        let subagents = Arc::new(SubagentExecutor::new(
            store.clone(),
            DurableStore::in_memory().expect("durable"),
            AgentRuntime::new(driver, builtin_registry()),
            Workspace::new(directory.path()).expect("workspace"),
            CapabilityPolicy::new(Some(Decision::Allow)),
            "scripted".to_owned(),
            HostContext::default(),
        ));
        let parent_conversation = store
            .create_conversation(
                "local-user",
                &NewConversation {
                    title: Some("Parent".to_owned()),
                    ..NewConversation::default()
                },
            )
            .expect("parent conversation")
            .id;
        let context = ToolContext::new(
            Workspace::new(directory.path()).expect("workspace"),
            CapabilityPolicy::new(Some(Decision::Allow)),
        )
        .with_actor("local-user".to_owned())
        .with_conversation(Some(parent_conversation));
        Fixture {
            store,
            subagents,
            context,
            parent_conversation,
            _directory: directory,
        }
    }

    #[tokio::test]
    async fn background_spawn_returns_immediately_and_collect_waits_terminal() {
        let fixture = fixture(Arc::new(ScriptedDriver::echo()));
        let spawn = SpawnSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        };
        let spawned = spawn
            .execute(
                &fixture.context,
                json!({"prompt": "background task", "background": true}),
            )
            .await
            .expect("spawn");
        assert!(!spawned.is_error, "{:?}", spawned.output);
        let run_id = spawned.output["subagentRunId"]
            .as_i64()
            .expect("subagentRunId");
        assert_eq!(spawned.output["background"], json!(true));

        let collect = CollectSubagent {
            subagents: Some(fixture.subagents.clone()),
        };
        let collected = collect
            .execute(
                &fixture.context,
                json!({"subagentRunId": run_id, "wait": true, "timeoutSecs": 10}),
            )
            .await
            .expect("collect");
        assert_eq!(collected.output["status"], json!("completed"));
        // The echo provider answers with the last user message.
        assert_eq!(collected.output["result"], json!("background task"));
    }

    #[tokio::test]
    async fn collect_wait_false_reports_the_live_status() {
        let fixture = fixture(Arc::new(ScriptedDriver::echo_with_delay(
            Duration::from_millis(400),
        )));
        let spawned = SpawnSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(
            &fixture.context,
            json!({"prompt": "slow task", "background": true}),
        )
        .await
        .expect("spawn");
        let run_id = spawned.output["subagentRunId"].as_i64().expect("id");
        let collected = CollectSubagent {
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(
            &fixture.context,
            json!({"subagentRunId": run_id, "wait": false}),
        )
        .await
        .expect("collect");
        assert!(
            matches!(
                collected.output["status"].as_str(),
                Some("queued" | "running")
            ),
            "live status reported: {:?}",
            collected.output
        );
    }

    #[tokio::test]
    async fn send_to_subagent_appends_a_steer_to_the_child_conversation() {
        let fixture = fixture(Arc::new(ScriptedDriver::echo_with_delay(
            Duration::from_millis(400),
        )));
        let spawned = SpawnSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(
            &fixture.context,
            json!({"prompt": "running", "background": true}),
        )
        .await
        .expect("spawn");
        let run_id = spawned.output["subagentRunId"].as_i64().expect("id");

        let sent = SendToSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(
            &fixture.context,
            json!({"subagentRunId": run_id, "message": "also check the tests"}),
        )
        .await
        .expect("send");
        assert_eq!(
            sent.output["delivered"],
            json!("steer"),
            "{:?}",
            sent.output
        );

        let run = fixture
            .subagents
            .get_run("local-user", run_id)
            .expect("run");
        let messages = fixture
            .store
            .list_messages(
                "local-user",
                run.conversation_id,
                &cool_store::domains::conversations::MessagePage {
                    limit: Some(20),
                    ..Default::default()
                },
            )
            .expect("child transcript");
        assert!(
            messages.iter().any(|message| message.role == "user"
                && message.content.as_deref() == Some("also check the tests")),
            "steer persisted in the child conversation: {messages:?}"
        );
    }

    #[tokio::test]
    async fn send_to_a_finished_subagent_fails() {
        let fixture = fixture(Arc::new(ScriptedDriver::echo()));
        let spawned = SpawnSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(&fixture.context, json!({"prompt": "quick"}))
        .await
        .expect("spawn");
        let run_id = spawned.output["subagentRunId"].as_i64().expect("id");
        let sent = SendToSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(
            &fixture.context,
            json!({"subagentRunId": run_id, "message": "too late"}),
        )
        .await
        .expect("send");
        assert!(sent.is_error);
        assert_eq!(sent.error_code.as_deref(), Some("subagent_finished"));
    }

    #[tokio::test]
    async fn list_subagents_returns_this_conversations_runs() {
        let fixture = fixture(Arc::new(ScriptedDriver::echo()));
        let spawned = SpawnSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(&fixture.context, json!({"prompt": "listed"}))
        .await
        .expect("spawn");
        let run_id = spawned.output["subagentRunId"].as_i64().expect("id");

        let listed = ListSubagents {
            store: fixture.store.clone(),
        }
        .execute(&fixture.context, json!({}))
        .await
        .expect("list");
        let runs = listed.output["runs"].as_array().expect("runs array");
        assert!(
            runs.iter()
                .any(|run| run["subagentRunId"].as_i64() == Some(run_id)),
            "spawned run listed for the parent conversation: {runs:?}"
        );
    }

    #[tokio::test]
    async fn spawn_at_max_depth_is_rejected() {
        let fixture = fixture(Arc::new(ScriptedDriver::echo()));
        let context = fixture.context.clone().with_spawn_depth(MAX_SPAWN_DEPTH);
        let result = SpawnSubagent {
            store: fixture.store.clone(),
            subagents: Some(fixture.subagents.clone()),
        }
        .execute(&context, json!({"prompt": "too deep"}))
        .await
        .expect("spawn");
        assert!(result.is_error);
        assert_eq!(result.error_code.as_deref(), Some("spawn_depth_exceeded"));
        assert!(
            fixture
                .store
                .list_subagent_runs(
                    "local-user",
                    &SubagentRunFilter {
                        parent_conversation_id: Some(fixture.parent_conversation),
                        status: None,
                        research_run_id: None,
                        limit: Some(10),
                    },
                )
                .expect("runs")
                .is_empty(),
            "no child was spawned past the depth limit"
        );
    }
}
