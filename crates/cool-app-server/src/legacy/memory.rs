//! Memory, entity, wiki, plans, subagents, research and artifact families.

use cool_protocol::*;
use cool_store::domains::artifacts::NewArtifact;
use cool_store::domains::conversations::{MessagePage, NewConversation};
use cool_store::domains::memory::{
    EntityPatch, MemoryFilter, MemoryItemPatch, NewEntity, NewMemoryItem,
};
use cool_store::domains::subagents::{
    NewSubagentRole, NewSubagentRun, SubagentRolePatch, SubagentRunFilter,
};
use cool_store::domains::wiki::{NewWikiArticle, WikiArticlePatch, WikiFilter};
use cool_store::{LegacyStore, StoreError};
use serde_json::json;

use super::{Unhandled, bridge, convert, fingerprint, idempotent, invalid_input, store_error};

pub(super) async fn dispatch(
    store: &LegacyStore,
    actor: &ActorRef,
    command: Command,
) -> Result<ResponsePayload, Unhandled> {
    let payload = match command {
        Command::MemoryList(params) => {
            let filter = MemoryFilter {
                memory_type: params.memory_type.clone(),
                scope: params.scope.clone(),
                status: params.status.clone(),
                conversation_id: params.conversation_id,
                pinned: params.pinned,
                limit: Some(usize::from(params.limit)),
                offset: params.offset as usize,
            };
            let records = store
                .list_memory_items(&actor.id, &filter)
                .map_err(store_error)?;
            ResponsePayload::MemoryListed(convert(records)?)
        }
        Command::MemoryGet(params) => ResponsePayload::MemoryGot(convert(
            store
                .get_memory_item(&actor.id, params.id)
                .map_err(store_error)?,
        )?),
        Command::MemoryCreate(params) => {
            let new = NewMemoryItem {
                scope: params.scope.clone().unwrap_or_else(|| "global".to_owned()),
                agent_id: params.agent_id,
                conversation_id: params.conversation_id,
                memory_type: params
                    .memory_type
                    .clone()
                    .unwrap_or_else(|| "semantic".to_owned()),
                content: params.content.clone(),
                structured: params.structured.clone(),
                tags: params.tags.clone(),
                importance: params.importance.unwrap_or(0.5),
                confidence: params.confidence.unwrap_or(0.7),
                source: params
                    .source
                    .clone()
                    .unwrap_or_else(|| "user_explicit".to_owned()),
                status: params.status.clone(),
                confirmed: params.confirmed,
                supersedes_id: params.supersedes_id,
                ttl_days: params.ttl_days,
                valid_from: params.valid_from.clone(),
                valid_to: params.valid_to.clone(),
                pinned: params.pinned,
            };
            let created = idempotent(
                store,
                actor,
                "memory.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_memory_item(&actor.id, &new),
            )?;
            ResponsePayload::MemoryCreated(convert(created)?)
        }
        Command::MemoryUpdate(params) => {
            let patch: MemoryItemPatch = bridge(&params)?;
            let memory_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "memory.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_memory_item(&actor.id, memory_id, &patch),
            )?;
            ResponsePayload::MemoryUpdated(convert(updated)?)
        }
        Command::MemoryDelete(params) => {
            let memory_id = params.id;
            let hard = params.hard;
            let deleted = idempotent(
                store,
                actor,
                "memory.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_memory_item(&actor.id, memory_id, hard)?;
                    Ok(LegacyOkResult { ok: true })
                },
            )?;
            ResponsePayload::MemoryDeleted(deleted)
        }
        Command::MemoryPending(params) => {
            let offset = params.offset as usize;
            let requested = usize::from(params.limit).saturating_add(offset);
            let mut records = store
                .list_pending_items(&actor.id, Some(requested))
                .map_err(store_error)?;
            if offset > 0 {
                records = records.into_iter().skip(offset).collect();
            }
            ResponsePayload::MemoryPending(convert(records)?)
        }
        Command::MemoryConfirm(params) => {
            let memory_id = params.id;
            let confirmed = idempotent(
                store,
                actor,
                "memory.confirm",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.confirm_memory_item(&actor.id, memory_id),
            )?;
            ResponsePayload::MemoryConfirmed(convert(confirmed)?)
        }
        Command::MemoryReject(params) => {
            let memory_id = params.id;
            let rejected = idempotent(
                store,
                actor,
                "memory.reject",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.reject_memory_item(&actor.id, memory_id)?;
                    Ok(LegacyOkResult { ok: true })
                },
            )?;
            ResponsePayload::MemoryRejected(rejected)
        }
        Command::MemoryPin(params) => {
            let memory_id = params.id;
            let pinned = params.pinned;
            let record = idempotent(
                store,
                actor,
                "memory.pin",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.pin_memory_item(&actor.id, memory_id, pinned),
            )?;
            ResponsePayload::MemoryPinned(convert(record)?)
        }
        Command::MemoryExplain(params) => ResponsePayload::MemoryExplained(convert(
            store
                .explain_memory(&actor.id, params.id)
                .map_err(store_error)?,
        )?),
        Command::MemoryEpisodes(params) => {
            let mut records = store
                .list_episodes(&actor.id, Some(usize::from(params.limit)))
                .map_err(store_error)?;
            if let Some(agent_id) = params.agent_id {
                records.retain(|episode| episode.agent_id == Some(agent_id));
            }
            ResponsePayload::MemoryEpisodes(convert(records)?)
        }
        Command::MemoryStats(_) => ResponsePayload::MemoryStats(convert(
            store.memory_stats(&actor.id).map_err(store_error)?,
        )?),
        Command::EntitiesList(params) => ResponsePayload::EntitiesListed(convert(
            store
                .list_entities(
                    &actor.id,
                    params.query.as_deref(),
                    params.entity_type.as_deref(),
                    Some(usize::from(params.limit)),
                )
                .map_err(store_error)?,
        )?),
        Command::EntitiesGet(params) => ResponsePayload::EntitiesGot(convert(
            store
                .get_entity(&actor.id, params.id)
                .map_err(store_error)?,
        )?),
        Command::EntitiesCreate(params) => {
            let new = NewEntity {
                name: params.name.clone(),
                entity_type: params.entity_type.clone(),
                aliases: params.aliases.clone(),
                attributes: params.attributes.clone(),
                description: params.description.clone(),
            };
            let created = idempotent(
                store,
                actor,
                "entities.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_entity(&actor.id, &new),
            )?;
            ResponsePayload::EntitiesCreated(convert(created)?)
        }
        Command::EntitiesUpdate(params) => {
            let patch = EntityPatch {
                name: params.name.clone(),
                entity_type: params.entity_type.clone(),
                aliases: params.aliases.clone(),
                attributes: params.attributes.clone(),
                description: params.description.clone(),
            };
            let entity_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "entities.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_entity(&actor.id, entity_id, &patch),
            )?;
            ResponsePayload::EntitiesUpdated(convert(updated)?)
        }
        Command::EntitiesDelete(params) => {
            let entity_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "entities.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_entity(&actor.id, entity_id)?;
                    Ok(LegacyOkResult { ok: true })
                },
            )?;
            ResponsePayload::EntitiesDeleted(deleted)
        }
        Command::WikiList(params) => {
            let filter = WikiFilter {
                archived: Some(params.archived.unwrap_or(false)),
                category: params.category.clone(),
                project_key: params.project_key.clone(),
                search: params.search.clone(),
                tag: params.tag.clone(),
                pinned: params.pinned,
                limit: Some(usize::from(params.limit)),
                offset: params.offset as usize,
            };
            let records = store
                .list_articles(&actor.id, &filter)
                .map_err(store_error)?;
            ResponsePayload::WikiListed(convert(records)?)
        }
        Command::WikiSearch(params) => {
            let filter = WikiFilter {
                search: Some(params.query.clone()),
                limit: Some(usize::from(params.limit)),
                ..WikiFilter::default()
            };
            let records = store
                .list_articles(&actor.id, &filter)
                .map_err(store_error)?;
            ResponsePayload::WikiSearched(convert(records)?)
        }
        Command::WikiGet(params) => ResponsePayload::WikiGot(convert(
            store
                .get_article(&actor.id, params.id)
                .map_err(store_error)?,
        )?),
        Command::WikiCreate(params) => {
            let new = NewWikiArticle {
                title: params.title.clone(),
                content: params.content.clone(),
                category: params
                    .category
                    .clone()
                    .unwrap_or_else(|| "general".to_owned()),
                tags: params.tags.clone(),
                source: params.source.clone().unwrap_or_else(|| "manual".to_owned()),
                source_memory_id: params.source_memory_id,
                project_key: params.project_key.clone(),
                metadata: params.metadata.clone(),
            };
            let created = idempotent(
                store,
                actor,
                "wiki.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_article(&actor.id, &new),
            )?;
            ResponsePayload::WikiCreated(convert(created)?)
        }
        Command::WikiUpdate(params) => {
            let patch = WikiArticlePatch {
                title: params.title.clone(),
                content: params.content.clone(),
                category: params.category.clone(),
                tags: params.tags.clone(),
                is_pinned: params.is_pinned,
                is_archived: params.is_archived,
            };
            let article_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "wiki.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_article(&actor.id, article_id, &patch),
            )?;
            ResponsePayload::WikiUpdated(convert(updated)?)
        }
        Command::WikiDelete(params) => {
            let article_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "wiki.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_article(&actor.id, article_id)?;
                    Ok(DeletedResult {
                        deleted: article_id,
                    })
                },
            )?;
            ResponsePayload::WikiDeleted(deleted)
        }
        Command::WikiCategories(_) => {
            let categories = store
                .wiki_categories(&actor.id, false)
                .map_err(store_error)?;
            ResponsePayload::WikiCategories(categories.into_iter().map(|(name, _)| name).collect())
        }
        Command::WikiStats(_) => {
            let stats = store.wiki_stats(&actor.id).map_err(store_error)?;
            ResponsePayload::WikiStats(WikiStatsRecord {
                total: stats.total,
                pinned: stats.pinned,
                archived: stats.archived,
                by_category: stats.by_category.into_iter().collect(),
            })
        }
        Command::WikiPromote(params) => {
            let new = NewWikiArticle {
                title: params.title.clone(),
                content: params.content.clone(),
                category: params
                    .category
                    .clone()
                    .unwrap_or_else(|| "general".to_owned()),
                tags: params.tags.clone(),
                source: "memory".to_owned(),
                source_memory_id: Some(params.memory_item_id),
                project_key: None,
                metadata: None,
            };
            let created = idempotent(
                store,
                actor,
                "wiki.promote",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_article(&actor.id, &new),
            )?;
            ResponsePayload::WikiPromoted(convert(created)?)
        }
        Command::PlansList(params) => ResponsePayload::PlansListed(convert(
            store
                .list_plans(&actor.id, params.conversation_id)
                .map_err(store_error)?,
        )?),
        Command::PlansGet(params) => ResponsePayload::PlansGot(convert(
            store
                .get_plan(&actor.id, params.conversation_id, params.plan_id)
                .map_err(store_error)?,
        )?),
        Command::PlansUpdate(params) => {
            let conversation_id = params.conversation_id;
            let plan_id = params.plan_id;
            let title = params.title.clone();
            let steps = params.steps.clone();
            let updated = idempotent(
                store,
                actor,
                "plans.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.update_plan_draft(
                        &actor.id,
                        conversation_id,
                        plan_id,
                        title.as_deref(),
                        steps.as_ref(),
                    )
                },
            )?;
            ResponsePayload::PlansUpdated(convert(updated)?)
        }
        Command::PlansApprove(params) => {
            let conversation_id = params.conversation_id;
            let plan_id = params.plan_id;
            let approved = params.approved;
            let updated = idempotent(
                store,
                actor,
                "plans.approve",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let status = if approved { "approved" } else { "cancelled" };
                    store.set_plan_status(&actor.id, conversation_id, plan_id, status)
                },
            )?;
            ResponsePayload::PlansApproved(convert(updated)?)
        }
        Command::PlansCancel(params) => {
            let conversation_id = params.conversation_id;
            let plan_id = params.plan_id;
            let updated = idempotent(
                store,
                actor,
                "plans.cancel",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.set_plan_status(&actor.id, conversation_id, plan_id, "cancelled"),
            )?;
            ResponsePayload::PlansCancelled(convert(updated)?)
        }
        Command::PlansTemplatesList(_) => ResponsePayload::PlansTemplatesListed(convert(
            store.list_plan_templates().map_err(store_error)?,
        )?),
        Command::PlansTemplatesCreate(params) => {
            let name = params.name.clone();
            let description = params.description.clone();
            let steps = params.steps.clone();
            let created = idempotent(
                store,
                actor,
                "plans.templates_create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_plan_template(&name, description.as_deref(), &steps),
            )?;
            ResponsePayload::PlansTemplatesCreated(convert(created)?)
        }
        Command::PlansTemplatesDelete(params) => {
            let template_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "plans.templates_delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_plan_template(template_id)?;
                    Ok(DeletedResult {
                        deleted: template_id,
                    })
                },
            )?;
            ResponsePayload::PlansTemplatesDeleted(deleted)
        }
        Command::SubagentsRolesList(_) => ResponsePayload::SubagentsRolesListed(convert(
            store.list_subagent_roles().map_err(store_error)?,
        )?),
        Command::SubagentsRolesGet(params) => ResponsePayload::SubagentsRolesGot(convert(
            store.get_subagent_role(params.id).map_err(store_error)?,
        )?),
        Command::SubagentsRolesCreate(params) => {
            let new = NewSubagentRole {
                name: params.name.clone(),
                description: params.description.clone(),
                system_prompt: params.system_prompt.clone(),
                model: params.model.clone(),
                tool_names: params.tool_names.clone(),
                capability_policy: params.capability_policy.clone(),
                max_iterations: params.max_iterations,
                max_cost_usd: params.max_cost_usd,
                is_builtin: params.is_builtin,
            };
            let created = idempotent(
                store,
                actor,
                "subagents.roles_create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_subagent_role(&new),
            )?;
            ResponsePayload::SubagentsRolesCreated(convert(created)?)
        }
        Command::SubagentsRolesUpdate(params) => {
            let patch = SubagentRolePatch {
                name: params.name.clone(),
                description: params.description.clone(),
                system_prompt: params.system_prompt.clone(),
                model: params.model.clone(),
                tool_names: params.tool_names.clone(),
                capability_policy: params.capability_policy.clone(),
                max_iterations: params.max_iterations,
                max_cost_usd: if params.clear_max_cost_usd {
                    Some(None)
                } else {
                    params.max_cost_usd.map(Some)
                },
            };
            let role_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "subagents.roles_update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_subagent_role(role_id, &patch),
            )?;
            ResponsePayload::SubagentsRolesUpdated(convert(updated)?)
        }
        Command::SubagentsRolesDelete(params) => {
            let role_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "subagents.roles_delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_subagent_role(role_id)?;
                    Ok(DeletedResult { deleted: role_id })
                },
            )?;
            ResponsePayload::SubagentsRolesDeleted(deleted)
        }
        Command::SubagentsLaunch(params) => {
            let parent = params.parent_conversation_id;
            let role_id = params.role_id;
            let profile_id = params.profile_id;
            let name = params.name.clone();
            let prompt = params.prompt.clone();
            let model = params.model.clone();
            let run = idempotent(
                store,
                actor,
                "subagents.launch",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    launch_subagent(
                        store, &actor.id, parent, role_id, profile_id, name, prompt, model,
                    )
                },
            )?;
            ResponsePayload::SubagentsLaunched(convert(run)?)
        }
        Command::SubagentsLaunchBatch(params) => {
            let parent = params.parent_conversation_id;
            let launches = params.items.clone();
            let runs = idempotent(
                store,
                actor,
                "subagents.launch_batch",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let mut runs = Vec::with_capacity(launches.len());
                    for launch in &launches {
                        runs.push(launch_subagent(
                            store,
                            &actor.id,
                            parent,
                            launch.role_id,
                            launch.profile_id,
                            launch.name.clone(),
                            launch.prompt.clone(),
                            launch.model.clone(),
                        )?);
                    }
                    Ok(runs)
                },
            )?;
            ResponsePayload::SubagentsLaunchedBatch(convert(runs)?)
        }
        Command::SubagentsRunsList(params) => {
            let filter = SubagentRunFilter {
                parent_conversation_id: params.parent_conversation_id,
                status: params.status.clone(),
                research_run_id: None,
                limit: Some(usize::from(params.limit)),
            };
            let records = store
                .list_subagent_runs(&actor.id, &filter)
                .map_err(store_error)?;
            ResponsePayload::SubagentsRunsListed(convert(records)?)
        }
        Command::SubagentsRunsGet(params) => {
            let run = store
                .get_subagent_run(&actor.id, params.id)
                .map_err(store_error)?;
            let messages = store
                .list_messages(
                    &actor.id,
                    run.conversation_id,
                    &MessagePage {
                        before_id: None,
                        after_id: None,
                        limit: Some(500),
                    },
                )
                .map_err(store_error)?;
            ResponsePayload::SubagentsRunsGot(SubagentRunDetailRecord {
                run: convert(run)?,
                messages: convert(messages)?,
            })
        }
        Command::SubagentsRunsCancel(params) => {
            let run_id = params.id;
            let run = idempotent(
                store,
                actor,
                "subagents.runs_cancel",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.finish_subagent_run(&actor.id, run_id, "cancelled", None, None, None),
            )?;
            ResponsePayload::SubagentsRunsCancelled(SubagentRunCancelResult {
                run_id: run.id,
                cancelled: run.status == "cancelled",
            })
        }
        Command::SubagentsRunsDelete(params) => {
            let run_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "subagents.runs_delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_subagent_run(&actor.id, run_id)?;
                    Ok(LegacyOkResult { ok: true })
                },
            )?;
            ResponsePayload::SubagentsRunsDeleted(deleted)
        }
        Command::ResearchList(params) => ResponsePayload::ResearchListed(convert(
            store
                .list_research_runs(&actor.id, Some(usize::from(params.limit)))
                .map_err(store_error)?,
        )?),
        Command::ResearchGet(params) => {
            let run = store
                .get_research_run(&actor.id, params.id)
                .map_err(store_error)?;
            let sub_questions = run
                .sub_questions
                .as_ref()
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let detail = ResearchRunDetailRecord {
                conversation_id: run.conversation_id,
                parent_task_run_id: run.parent_task_run_id,
                sub_questions,
                sources: run
                    .sources
                    .as_ref()
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                citations: run
                    .citations
                    .as_ref()
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                report_markdown: run.report_markdown.clone(),
                run: convert(run)?,
            };
            ResponsePayload::ResearchGot(detail)
        }
        Command::ResearchCreate(params) => {
            let topic = params.topic.trim().to_owned();
            if topic.is_empty() {
                return Err(invalid_input("Topic is required").into());
            }
            let depth = params.depth.clamp(3, 5);
            let new = cool_store::domains::research::NewResearchRun {
                topic,
                depth,
                model: params.model.clone(),
                conversation_id: params.conversation_id,
                parent_task_run_id: None,
            };
            let created = idempotent(
                store,
                actor,
                "research.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_research_run(&actor.id, &new),
            )?;
            ResponsePayload::ResearchCreated(convert(created)?)
        }
        Command::ResearchCancel(params) => {
            let run_id = params.id;
            let cancelled = idempotent(
                store,
                actor,
                "research.cancel",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.cancel_research_run(&actor.id, run_id),
            )?;
            ResponsePayload::ResearchCancelled(ResearchCancelResult {
                cancelled: cancelled.id,
            })
        }
        Command::ResearchRerun(params) => {
            let run_id = params.id;
            let depth = params.depth;
            let model = params.model.clone();
            let rerun = idempotent(
                store,
                actor,
                "research.rerun",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let original = store.get_research_run(&actor.id, run_id)?;
                    store.create_research_run(
                        &actor.id,
                        &cool_store::domains::research::NewResearchRun {
                            topic: original.topic.clone(),
                            depth: depth.unwrap_or(original.depth).clamp(3, 5),
                            model: model.clone().or_else(|| original.model.clone()),
                            conversation_id: original.conversation_id,
                            parent_task_run_id: None,
                        },
                    )
                },
            )?;
            ResponsePayload::ResearchReran(convert(rerun)?)
        }
        Command::ArtifactsList(params) => ResponsePayload::ArtifactsListed(convert(
            store
                .list_artifacts_filtered(
                    &actor.id,
                    params.conversation_id,
                    params.run_id,
                    params.kind.as_deref(),
                    params.include_deleted,
                    Some(usize::from(params.limit)),
                )
                .map_err(store_error)?,
        )?),
        Command::ArtifactsGet(params) => {
            let artifact = store
                .get_artifact(&actor.id, params.artifact_id)
                .map_err(store_error)?;
            if artifact.conversation_id != params.conversation_id {
                return Err(store_error(StoreError::NotFound("artifact")).into());
            }
            let versions = store
                .list_artifacts_filtered(
                    &actor.id,
                    params.conversation_id,
                    None,
                    None,
                    true,
                    Some(500),
                )
                .map_err(store_error)?
                .into_iter()
                .filter(|candidate| {
                    candidate.id == artifact.id || candidate.parent_id == Some(artifact.id)
                })
                .collect::<Vec<_>>();
            let detail = ArtifactDetailRecord {
                extracted_text: artifact.extracted_text.clone(),
                versions: convert(versions)?,
                artifact: convert(artifact)?,
            };
            ResponsePayload::ArtifactsGot(detail)
        }
        Command::ArtifactsDelete(params) => {
            let artifact_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "artifacts.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.soft_delete_artifact(&actor.id, artifact_id)?;
                    Ok(DeletedResult {
                        deleted: artifact_id,
                    })
                },
            )?;
            ResponsePayload::ArtifactsDeleted(deleted)
        }
        other => return Err(Unhandled::NotHandled(Box::new(other))),
    };
    Ok(payload)
}

#[allow(clippy::too_many_arguments)]
fn launch_subagent(
    store: &LegacyStore,
    actor_id: &str,
    parent_conversation_id: i64,
    role_id: Option<i64>,
    profile_id: Option<i64>,
    name: Option<String>,
    prompt: String,
    model: Option<String>,
) -> Result<cool_store::domains::subagents::SubagentRun, StoreError> {
    let title = name
        .clone()
        .unwrap_or_else(|| format!("Subagent: {}", prompt.chars().take(40).collect::<String>()));
    let child = store.create_conversation(
        actor_id,
        &NewConversation {
            title: Some(title),
            model,
            metadata: Some(json!({
                "is_subagent": true,
                "parent_conversation_id": parent_conversation_id,
            })),
            ..NewConversation::default()
        },
    )?;
    store.create_subagent_run(
        actor_id,
        parent_conversation_id,
        &NewSubagentRun {
            role_id,
            parent_run_id: None,
            conversation_id: child.id,
            name,
            prompt,
            profile_id,
            research_run_id: None,
        },
    )
}

// Artifact registration is reachable through the store's typed path; the
// protocol upload exception keeps raw blob transport out of the command set.
#[allow(dead_code)]
fn register_artifact_type(_: &NewArtifact) {}
