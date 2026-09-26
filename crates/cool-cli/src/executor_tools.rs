//! Executor-bound agent tools (M12): `deep_research`, `spawn_subagent`, and
//! the skill trio. Unlike the store tools these need server-owned handles —
//! the subagent/research executors and the skills admin — so they are built
//! after `build_server` and registered onto the shared [`ToolRegistry`]
//! (clones share the backing map, so the live agent runtime sees them
//! immediately).

use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{Tool, ToolContext, ToolError, ToolHandler, ToolResult};
use cool_app_server::{ResearchExecutor, SkillAdmin, SubagentExecutor, SubagentLaunchSpec};
use cool_protocol::SkillCreateParams;
use cool_security::{Capability, Decision};
use cool_store::LegacyStore;
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
        ),
        Tool::new(
            definition(
                "spawn_subagent",
                "Spawn a subagent to handle a sub-task and return its result. Optionally bind a role name or agent profile slug.",
                json!({"type":"object","properties":{"prompt":{"type":"string","description":"Task for the subagent"},"role":{"type":"string","description":"Subagent role name"},"profile":{"type":"string","description":"Agent profile slug (takes precedence over role)"},"model":{"type":"string","description":"Optional model override"}},"required":["prompt"],"additionalProperties":false}),
            ),
            [Capability::Execute],
            Decision::Ask,
            SpawnSubagent { store, subagents },
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
        ),
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
        reject_unknown(&arguments, &["prompt", "role", "profile", "model"])?;
        let Some(executor) = &self.subagents else {
            return Ok(ToolResult::error(
                "executor_unavailable",
                "subagent executor is not available on this server",
            ));
        };
        let prompt = required_string(&arguments, "prompt")?.to_owned();
        let role = optional_string(&arguments, "role")?;
        let profile = optional_string(&arguments, "profile")?;
        let model = optional_string(&arguments, "model")?;

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
        };
        // Python parity: a fresh key per call — a subagent launch must never
        // dedupe against an earlier spawn.
        let key = format!("spawn-subagent:{}", Uuid::new_v4());
        let run = executor
            .launch(&context.actor_id, spec, &key, &key)
            .await
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;

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
