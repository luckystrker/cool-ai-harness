//! Admin families: providers, budgets, analytics, tasks, RSS, webhooks and
//! profiles.

use std::path::Path;

use cool_protocol::*;
use cool_security::SecretKeyring;
use cool_store::domains::budgets::BudgetUpdate;
use cool_store::domains::conversations::{MessagePage, NewConversation, NewMessage};
use cool_store::domains::profiles::{AgentProfilePatch, NewAgentProfile};
use cool_store::domains::providers::{NewProvider, Provider, ProviderPatch};
use cool_store::domains::rss::NewRssSubscription;
use cool_store::domains::tasks::{NewScheduledTask, NewTaskRun, ScheduledTask, ScheduledTaskPatch};
use cool_store::domains::webhooks::{NewWebhookEndpoint, NewWebhookEvent, WebhookEndpointPatch};
use cool_store::{LegacyStore, StoreError};
use serde_json::Value;

use super::{
    Unhandled, bridge, convert, encrypt_secret, fingerprint, idempotent, invalid_input, store_error,
};

const DEFAULT_SOURCE_TYPE: &str = "generic";
const DOW_LABELS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

pub(super) async fn dispatch(
    store: &LegacyStore,
    secrets: Option<&SecretKeyring>,
    _workspace_root: &Path,
    actor: &ActorRef,
    command: Command,
) -> Result<ResponsePayload, Unhandled> {
    let payload = match command {
        Command::ProvidersList(params) => ResponsePayload::ProvidersListed(convert(
            store
                .list_providers(&actor.id, params.include_inactive)
                .map_err(store_error)?,
        )?),
        Command::ProvidersCreate(params) => {
            let encrypted = params
                .api_key
                .as_deref()
                .map(|key| encrypt_secret(secrets, key))
                .transpose()?;
            let new = NewProvider {
                name: params.name.clone(),
                label: params.label.clone(),
                base_url: params.base_url.clone(),
                api_key_encrypted: encrypted,
                default_model: params.default_model.clone(),
                is_active: params.is_active,
                is_subscription: params.is_subscription,
                is_fallback: params.is_fallback,
                chat_models: params.chat_models.clone(),
            };
            let created = idempotent(
                store,
                actor,
                "providers.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let provider = store.create_provider(&actor.id, &new)?;
                    if params.is_default {
                        return store.set_default_provider(&actor.id, provider.id);
                    }
                    Ok(provider)
                },
            )?;
            ResponsePayload::ProvidersCreated(convert(created)?)
        }
        Command::ProvidersGet(params) => ResponsePayload::ProvidersGot(convert(
            store
                .get_provider(&actor.id, params.id)
                .map_err(store_error)?,
        )?),
        Command::ProvidersUpdate(params) => {
            let encrypted = params
                .api_key
                .as_deref()
                .map(|key| encrypt_secret(secrets, key))
                .transpose()?;
            let patch = ProviderPatch {
                label: params.label.clone(),
                base_url: params.base_url.clone(),
                api_key_encrypted: encrypted,
                default_model: params.default_model.clone(),
                is_active: params.is_active,
                is_fallback: params.is_fallback,
                chat_models: params.chat_models.clone(),
                is_default: params.is_default,
            };
            let provider_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "providers.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_provider(&actor.id, provider_id, &patch),
            )?;
            ResponsePayload::ProvidersUpdated(convert(updated)?)
        }
        Command::ProvidersDelete(params) => {
            let provider_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "providers.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_provider(&actor.id, provider_id)?;
                    Ok(DeletedResult {
                        deleted: provider_id,
                    })
                },
            )?;
            ResponsePayload::ProvidersDeleted(deleted)
        }
        Command::ProvidersModels(params) => {
            let provider = store
                .get_provider(&actor.id, params.id)
                .map_err(store_error)?;
            ResponsePayload::ProvidersModels(provider_models(&provider))
        }
        Command::BudgetsGet(_) => ResponsePayload::BudgetsGot(convert(
            store
                .budget_status(&actor.id, super::now_seconds())
                .map_err(store_error)?,
        )?),
        Command::BudgetsUpdate(params) => {
            let update = BudgetUpdate {
                daily_limit_usd: params.daily_limit_usd,
                weekly_limit_usd: params.weekly_limit_usd,
                monthly_limit_usd: params.monthly_limit_usd,
                alert_threshold_pct: params.alert_threshold_pct,
                block_on_exceed: params.block_on_exceed,
            };
            let updated = idempotent(
                store,
                actor,
                "budgets.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.upsert_budget(&actor.id, &update)?;
                    store.budget_status(&actor.id, super::now_seconds())
                },
            )?;
            ResponsePayload::BudgetsUpdated(convert(updated)?)
        }
        Command::BudgetsOverrideSet(params) => {
            let until = cool_store::parse_python_datetime(&params.until)
                .ok_or_else(|| invalid_input("override `until` must be an ISO-8601 timestamp"))?;
            if until <= super::now_seconds() {
                return Err(invalid_input("override `until` must be in the future").into());
            }
            let until_text = params.until.clone();
            let updated = idempotent(
                store,
                actor,
                "budgets.override_set",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.set_budget_override(&actor.id, Some(&until_text))?;
                    store.budget_status(&actor.id, super::now_seconds())
                },
            )?;
            ResponsePayload::BudgetsOverrideSet(convert(updated)?)
        }
        Command::BudgetsOverrideClear(params) => {
            let cleared = idempotent(
                store,
                actor,
                "budgets.override_clear",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.set_budget_override(&actor.id, None)?;
                    store.budget_status(&actor.id, super::now_seconds())
                },
            )?;
            ResponsePayload::BudgetsOverrideCleared(convert(cleared)?)
        }
        Command::BudgetsSpend(params) => {
            let since = params
                .since
                .as_deref()
                .map(|value| {
                    cool_store::parse_python_datetime(value)
                        .ok_or_else(|| invalid_input("`since` must be an ISO-8601 timestamp"))
                })
                .transpose()?;
            let records = store
                .list_spend(&actor.id, since, Some(usize::from(params.limit)))
                .map_err(store_error)?;
            ResponsePayload::BudgetsSpend(convert(records)?)
        }
        Command::AnalyticsSummary(params) => ResponsePayload::AnalyticsSummary(convert(
            store
                .summary(&actor.id, i64::from(params.days))
                .map_err(store_error)?,
        )?),
        Command::AnalyticsSpendOverTime(params) => {
            ResponsePayload::AnalyticsSpendOverTime(convert(
                store
                    .spend_over_time(&actor.id, i64::from(params.days), &params.bucket)
                    .map_err(store_error)?,
            )?)
        }
        Command::AnalyticsSpendByModel(params) => ResponsePayload::AnalyticsSpendByModel(convert(
            store
                .spend_by_model(&actor.id, i64::from(params.days))
                .map_err(store_error)?,
        )?),
        Command::AnalyticsTopTools(params) => ResponsePayload::AnalyticsTopTools(convert(
            store
                .top_tools(&actor.id, i64::from(params.days), usize::from(params.limit))
                .map_err(store_error)?,
        )?),
        Command::AnalyticsLatency(params) => ResponsePayload::AnalyticsLatency(convert(
            store
                .latency(&actor.id, i64::from(params.days), &params.bucket)
                .map_err(store_error)?,
        )?),
        Command::AnalyticsCallHistory(params) => {
            let rows = store
                .call_history(
                    &actor.id,
                    usize::from(params.limit),
                    params.offset as usize,
                    params.model.as_deref(),
                    params.provider.as_deref(),
                )
                .map_err(store_error)?;
            let total = store
                .call_history_total(
                    &actor.id,
                    params.model.as_deref(),
                    params.provider.as_deref(),
                )
                .map_err(store_error)?;
            ResponsePayload::AnalyticsCallHistory(CallHistoryResult {
                rows: convert(rows)?,
                total,
            })
        }
        Command::AnalyticsMemoryActivity(params) => {
            ResponsePayload::AnalyticsMemoryActivity(convert(
                store
                    .memory_activity(&actor.id, i64::from(params.days), &params.bucket)
                    .map_err(store_error)?,
            )?)
        }
        Command::TasksList(params) => {
            let mut tasks = store
                .list_tasks(&actor.id, params.enabled == Some(true))
                .map_err(store_error)?;
            if params.enabled == Some(false) {
                tasks.retain(|task| !task.enabled);
            }
            ResponsePayload::TasksListed(convert(tasks)?)
        }
        Command::TasksGet(params) => ResponsePayload::TasksGot(convert(
            store.get_task(&actor.id, params.id).map_err(store_error)?,
        )?),
        Command::TasksCreate(params) => {
            let new: NewScheduledTask = bridge(&params)?;
            let created = idempotent(
                store,
                actor,
                "tasks.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_task(&actor.id, &new),
            )?;
            ResponsePayload::TasksCreated(convert(created)?)
        }
        Command::TasksUpdate(params) => {
            let patch: ScheduledTaskPatch = bridge(&params)?;
            let task_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "tasks.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_task(&actor.id, task_id, &patch),
            )?;
            ResponsePayload::TasksUpdated(convert(updated)?)
        }
        Command::TasksDelete(params) => {
            let task_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "tasks.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_task(&actor.id, task_id)?;
                    Ok(DeletedResult { deleted: task_id })
                },
            )?;
            ResponsePayload::TasksDeleted(deleted)
        }
        Command::TasksRun(params) => {
            let task_id = params.id;
            let run = idempotent(
                store,
                actor,
                "tasks.run",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let task = store.get_task(&actor.id, task_id)?;
                    store.create_task_run(
                        &actor.id,
                        task_id,
                        &NewTaskRun {
                            trigger_source: "manual".to_owned(),
                            prompt: task.prompt.clone(),
                            status: "queued".to_owned(),
                            approval_policy: Some(task.approval_policy.clone()),
                            approval_reason: None,
                            skip_reason: None,
                        },
                    )
                },
            )?;
            ResponsePayload::TasksRan(convert(run)?)
        }
        Command::TasksRunsList(params) => ResponsePayload::TasksRunsListed(convert(
            store
                .list_task_runs(&actor.id, params.task_id, Some(usize::from(params.limit)))
                .map_err(store_error)?,
        )?),
        Command::TasksRunsGet(params) => {
            let run = store
                .get_task_run(&actor.id, params.id)
                .map_err(store_error)?;
            let messages = match run.conversation_id {
                Some(conversation_id) => store
                    .list_messages(
                        &actor.id,
                        conversation_id,
                        &MessagePage {
                            before_id: None,
                            after_id: None,
                            limit: Some(500),
                        },
                    )
                    .map_err(store_error)?,
                None => Vec::new(),
            };
            ResponsePayload::TasksRunsGot(TaskRunDetailRecord {
                run: convert(run)?,
                messages: convert(messages)?,
            })
        }
        Command::TasksRunsCancel(params) => {
            let run_id = params.id;
            let run = idempotent(
                store,
                actor,
                "tasks.runs_cancel",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.cancel_task_run(&actor.id, run_id),
            )?;
            ResponsePayload::TasksRunsCancelled(TaskRunCancelResult {
                task_run_id: run.id,
                cancelled: run.status == "cancelled",
            })
        }
        Command::TasksRunsRead(params) => {
            let run_id = params.id;
            let is_read = params.is_read;
            let run = idempotent(
                store,
                actor,
                "tasks.runs_read",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.mark_task_run_read(&actor.id, run_id, is_read),
            )?;
            ResponsePayload::TasksRunsRead(convert(run)?)
        }
        Command::TasksInbox(params) => {
            let unread_count = store
                .count_unread_task_runs(&actor.id)
                .map_err(store_error)?;
            let runs = store
                .list_task_inbox(
                    &actor.id,
                    params.unread_only,
                    Some(usize::from(params.limit)),
                )
                .map_err(store_error)?;
            ResponsePayload::TasksInbox(TaskInboxResult {
                unread_count,
                runs: convert(runs)?,
            })
        }
        Command::TasksScheduler(_) => {
            let tasks = store.list_tasks(&actor.id, false).map_err(store_error)?;
            ResponsePayload::TasksScheduler(scheduler_status(&tasks))
        }
        Command::TasksParseCron(params) => {
            ResponsePayload::TasksParsedCron(parse_cron(&params.text))
        }
        Command::RssSubscriptionsList(params) => ResponsePayload::RssSubscriptionsListed(convert(
            store
                .list_subscriptions(&actor.id, params.category.as_deref(), params.enabled)
                .map_err(store_error)?,
        )?),
        Command::RssSubscribe(params) => {
            let new = NewRssSubscription {
                url: params.url.clone(),
                title: params.title.clone(),
                site_url: params.site_url.clone(),
                category: params.category.clone(),
                fetch_interval_minutes: params.fetch_interval_minutes,
                enabled: params.enabled,
            };
            let created = idempotent(
                store,
                actor,
                "rss.subscribe",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_subscription(&actor.id, &new),
            )?;
            ResponsePayload::RssSubscribed(convert(created)?)
        }
        Command::RssUnsubscribe(params) => {
            let subscription_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "rss.unsubscribe",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_subscription(&actor.id, subscription_id)?;
                    Ok(DeletedResult {
                        deleted: subscription_id,
                    })
                },
            )?;
            ResponsePayload::RssUnsubscribed(deleted)
        }
        Command::RssEntriesList(params) => ResponsePayload::RssEntriesListed(convert(
            store
                .list_entries(
                    &actor.id,
                    params.subscription_id,
                    Some(usize::from(params.limit)),
                    params.unread_only,
                )
                .map_err(store_error)?,
        )?),
        Command::RssEntriesAll(params) => ResponsePayload::RssEntriesAll(convert(
            store
                .list_all_entries(
                    &actor.id,
                    Some(usize::from(params.limit)),
                    params.unread_only,
                )
                .map_err(store_error)?,
        )?),
        Command::RssEntryRead(params) => {
            let entry_id = params.id;
            let is_read = params.is_read;
            let entry = idempotent(
                store,
                actor,
                "rss.entry_read",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.mark_entry_read(&actor.id, entry_id, is_read),
            )?;
            ResponsePayload::RssEntryRead(convert(entry)?)
        }
        Command::WebhooksList(_) => ResponsePayload::WebhooksListed(convert(
            store.list_endpoints(&actor.id).map_err(store_error)?,
        )?),
        Command::WebhooksGet(params) => ResponsePayload::WebhooksGot(convert(
            store
                .get_endpoint(&actor.id, params.id)
                .map_err(store_error)?,
        )?),
        Command::WebhooksCreate(params) => {
            let new = NewWebhookEndpoint {
                name: params.name.clone(),
                hook_id: None,
                secret: None,
                source_type: params
                    .source_type
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SOURCE_TYPE.to_owned()),
                event_filter: params.event_filter.clone(),
                task_id: params.task_id,
                prompt_template: params.prompt_template.clone(),
                enabled: params.enabled,
            };
            let created = idempotent(
                store,
                actor,
                "webhooks.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_endpoint(&actor.id, &new),
            )?;
            ResponsePayload::WebhooksCreated(convert(created)?)
        }
        Command::WebhooksUpdate(params) => {
            let patch = WebhookEndpointPatch {
                name: params.name.clone(),
                source_type: params.source_type.clone(),
                event_filter: params.event_filter.clone(),
                task_id: params.task_id,
                prompt_template: params.prompt_template.clone(),
                enabled: params.enabled,
            };
            let endpoint_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "webhooks.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_endpoint(&actor.id, endpoint_id, &patch),
            )?;
            ResponsePayload::WebhooksUpdated(convert(updated)?)
        }
        Command::WebhooksDelete(params) => {
            let endpoint_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "webhooks.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_endpoint(&actor.id, endpoint_id)?;
                    Ok(DeletedResult {
                        deleted: endpoint_id,
                    })
                },
            )?;
            ResponsePayload::WebhooksDeleted(deleted)
        }
        Command::WebhooksEvents(params) => ResponsePayload::WebhooksEvents(convert(
            store
                .list_webhook_events_by_status(
                    &actor.id,
                    params.endpoint_id,
                    params.status.as_deref(),
                    Some(usize::from(params.limit)),
                )
                .map_err(store_error)?,
        )?),
        Command::WebhooksReplay(params) => {
            let event_id = params.event_id;
            let endpoint_id = params.endpoint_id;
            let replayed = idempotent(
                store,
                actor,
                "webhooks.replay",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let event = store.get_webhook_event(&actor.id, event_id)?;
                    if event.endpoint_id != endpoint_id {
                        return Err(StoreError::NotFound("webhook event"));
                    }
                    store.record_webhook_event(
                        endpoint_id,
                        &NewWebhookEvent {
                            event_type: event.event_type.clone(),
                            payload: event.payload.clone(),
                            signature_valid: event.signature_valid,
                            status: Some("received"),
                        },
                    )
                },
            )?;
            ResponsePayload::WebhooksReplayed(convert(replayed)?)
        }
        Command::ProfilesList(params) => ResponsePayload::ProfilesListed(convert(
            store
                .list_profiles(params.include_inactive)
                .map_err(store_error)?,
        )?),
        Command::ProfilesGet(params) => ResponsePayload::ProfilesGot(convert(
            store.get_profile(params.id).map_err(store_error)?,
        )?),
        Command::ProfilesCreate(params) => {
            let new: NewAgentProfile = bridge(&params)?;
            let created = idempotent(
                store,
                actor,
                "profiles.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_profile(&new),
            )?;
            ResponsePayload::ProfilesCreated(convert(created)?)
        }
        Command::ProfilesUpdate(params) => {
            let patch: AgentProfilePatch = bridge(&params)?;
            let profile_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "profiles.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_profile(profile_id, &patch),
            )?;
            ResponsePayload::ProfilesUpdated(convert(updated)?)
        }
        Command::ProfilesDelete(params) => {
            let profile_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "profiles.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_profile(profile_id)?;
                    Ok(DeletedResult {
                        deleted: profile_id,
                    })
                },
            )?;
            ResponsePayload::ProfilesDeleted(deleted)
        }
        Command::ProfilesSeed(params) => {
            let created = idempotent(
                store,
                actor,
                "profiles.seed",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store
                        .seed_builtin_profiles()
                        .map(|created| SeedResult { created })
                },
            )?;
            ResponsePayload::ProfilesSeeded(created)
        }
        Command::ProfilesClone(params) => {
            let profile_id = params.id;
            let cloned = idempotent(
                store,
                actor,
                "profiles.clone",
                &params.idempotency_key,
                &fingerprint(&params),
                || clone_profile(store, profile_id),
            )?;
            ResponsePayload::ProfilesCloned(convert(cloned)?)
        }
        Command::ProfilesPlayground(params) => {
            let profile_id = params.id;
            let title = params.title.clone();
            let initial_prompt = params.initial_prompt.clone();
            let conversation_id = idempotent(
                store,
                actor,
                "profiles.playground",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let profile = store.get_profile(profile_id)?;
                    if !profile.is_active {
                        return Err(StoreError::NotFound("profile"));
                    }
                    let conversation = store.create_conversation(
                        &actor.id,
                        &NewConversation {
                            title: Some(
                                title
                                    .clone()
                                    .unwrap_or_else(|| format!("{} playground", profile.name)),
                            ),
                            model: profile.model.clone(),
                            profile_id: Some(profile.id),
                            ..NewConversation::default()
                        },
                    )?;
                    if let Some(prompt) = initial_prompt.as_deref() {
                        store.add_message(
                            &actor.id,
                            conversation.id,
                            &NewMessage {
                                role: "user".to_owned(),
                                content: Some(prompt.to_owned()),
                                ..NewMessage::default()
                            },
                        )?;
                    }
                    Ok(PlaygroundResult {
                        conversation_id: conversation.id,
                    })
                },
            )?;
            ResponsePayload::ProfilesPlayground(conversation_id)
        }
        Command::ConstructorMacros(params) => ResponsePayload::ConstructorMacros(convert(
            store
                .list_macro_tools(&actor.id, params.include_inactive)
                .map_err(store_error)?,
        )?),
        Command::ConstructorMacrosCreate(params) => {
            let new: cool_store::domains::constructor::NewMacroTool = bridge(&params)?;
            let created = idempotent(
                store,
                actor,
                "constructor.macros_create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_macro_tool(&actor.id, &new),
            )?;
            ResponsePayload::ConstructorMacrosCreated(convert(created)?)
        }
        Command::ConstructorMacrosUpdate(params) => {
            let patch: cool_store::domains::constructor::MacroToolPatch = bridge(&params)?;
            let macro_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "constructor.macros_update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_macro_tool(&actor.id, macro_id, &patch),
            )?;
            ResponsePayload::ConstructorMacrosUpdated(convert(updated)?)
        }
        Command::ConstructorMacrosDelete(params) => {
            let macro_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "constructor.macros_delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_macro_tool(&actor.id, macro_id)?;
                    Ok(DeletedResult { deleted: macro_id })
                },
            )?;
            ResponsePayload::ConstructorMacrosDeleted(deleted)
        }
        other => return Err(Unhandled::NotHandled(Box::new(other))),
    };
    Ok(payload)
}

fn provider_models(provider: &Provider) -> Vec<ModelInfoRecord> {
    let Some(items) = provider.chat_models.as_ref().and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::String(id) => Some(ModelInfoRecord {
                id: id.clone(),
                context_window: None,
                prompt_price: None,
                completion_price: None,
            }),
            Value::Object(_) => Some(ModelInfoRecord {
                id: item.get("id").and_then(Value::as_str)?.to_owned(),
                context_window: item.get("context_window").and_then(Value::as_i64),
                prompt_price: item.get("prompt_price").and_then(Value::as_f64),
                completion_price: item.get("completion_price").and_then(Value::as_f64),
            }),
            _ => None,
        })
        .collect()
}

fn clone_profile(
    store: &LegacyStore,
    profile_id: i64,
) -> Result<cool_store::domains::profiles::AgentProfile, StoreError> {
    let source = store.get_profile(profile_id)?;
    let base_slug = format!("{}-copy", source.slug);
    let mut slug = base_slug.clone();
    let mut suffix = 2;
    while store.find_profile_by_slug(&slug)?.is_some() {
        slug = format!("{base_slug}-{suffix}");
        suffix += 1;
    }
    store.create_profile(&NewAgentProfile {
        name: format!("{} Copy", source.name),
        slug,
        description: source.description.clone(),
        system_prompt: source.system_prompt.clone(),
        model: source.model.clone(),
        tool_names: source.tool_names.clone(),
        skill_names: source.skill_names.clone(),
        settings: source.settings.clone(),
        avatar_color: source.avatar_color.clone(),
        is_builtin: false,
        is_active: true,
        is_shared: false,
    })
}

fn scheduler_status(tasks: &[ScheduledTask]) -> SchedulerStatusRecord {
    let jobs = tasks
        .iter()
        .filter(|task| task.enabled)
        .map(|task| SchedulerJobRecord {
            id: task.id.to_string(),
            name: task.name.clone(),
            next_run_time: task.next_run_at.clone(),
        })
        .collect();
    SchedulerStatusRecord {
        enabled: true,
        running: false,
        timezone: "UTC".to_owned(),
        max_concurrent_tasks: 3,
        jobs,
    }
}

fn parse_cron(text: &str) -> ParseCronResult {
    let text = text.trim();
    let expression = if text.is_empty() {
        None
    } else if cool_store::scheduler::cron_next_runs(text, super::now_seconds(), 1).is_ok() {
        Some(text.to_owned())
    } else {
        parse_natural_schedule(text)
    };
    match expression {
        Some(expression) => {
            match cool_store::scheduler::cron_next_runs(&expression, super::now_seconds(), 3) {
                Ok(runs) => ParseCronResult {
                    cron_expression: Some(expression.clone()),
                    description: Some(describe_cron(&expression)),
                    next_runs: runs
                        .into_iter()
                        .map(|timestamp| {
                            format!(
                                "{}Z",
                                cool_store::python_datetime(timestamp, 0).replace(' ', "T")
                            )
                        })
                        .collect(),
                    detail: None,
                },
                Err(_) => ParseCronResult {
                    cron_expression: None,
                    description: None,
                    next_runs: Vec::new(),
                    detail: Some(format!("Could not interpret {text:?} as a schedule")),
                },
            }
        }
        None => ParseCronResult {
            cron_expression: None,
            description: None,
            next_runs: Vec::new(),
            detail: Some(format!("Could not interpret {text:?} as a schedule")),
        },
    }
}

/// Best-effort natural-language schedule port of `tasks/cron.py`'s
/// `parse_natural_schedule` for the recurring phrasings the UI/agent produce.
/// Returns a 5-field cron expression, or `None` when the phrase is unknown.
fn parse_natural_schedule(text: &str) -> Option<String> {
    let low = text.to_lowercase();
    let tokens = low.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if !matches!(*token, "every" | "each" | "каждые" | "каждый" | "каждую") {
            continue;
        }
        let Some(number) = tokens
            .get(index + 1)
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(unit) = tokens.get(index + 2) else {
            continue;
        };
        if unit.starts_with("min") || unit.starts_with("минут") {
            return Some(format!("*/{} * * * *", number.clamp(1, 59)));
        }
        if unit.starts_with("hour") || unit.starts_with("час") {
            return Some(format!("0 */{} * * *", number.clamp(1, 23)));
        }
    }
    let has = |needle: &str| low.contains(needle);
    if has("every minute") || has("каждую минуту") || has("ежеминутно") {
        return Some("* * * * *".to_owned());
    }
    if has("every hour") || has("hourly") || has("каждый час") || has("ежечасно") {
        return Some("0 * * * *".to_owned());
    }
    let time = extract_time(&low);
    let (hour, minute) = time.unwrap_or((9, 0));
    if has("weekday") || has("по будням") || has("в будни") {
        return Some(format!("{minute} {hour} * * 1-5"));
    }
    if has("weekend") || has("по выходным") || has("в выходные") {
        return Some(format!("{minute} {hour} * * 0,6"));
    }
    if let Some(day) = find_weekday(&low) {
        return Some(format!("{minute} {hour} * * {day}"));
    }
    if has("monthly") || has("ежемесячно") {
        let day = find_day_of_month(&low).unwrap_or(1).clamp(1, 28);
        return Some(format!("{minute} {hour} {day} * *"));
    }
    if has("weekly") || has("еженедельно") {
        return Some(format!("{minute} {hour} * * 1"));
    }
    if has("daily") || has("every day") || has("каждый день") || has("ежедневно")
    {
        return Some(format!("{minute} {hour} * * *"));
    }
    time.map(|_| format!("{minute} {hour} * * *"))
}

fn extract_time(text: &str) -> Option<(u32, u32)> {
    let tokens = text.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if let Some((hour_text, minute_text)) = token.split_once([':', '.']) {
            let (Ok(hour), Ok(minute)) = (hour_text.parse::<u32>(), minute_text.parse::<u32>())
            else {
                continue;
            };
            let hour = apply_meridiem(hour, tokens.get(index + 1).copied());
            if hour <= 23 && minute <= 59 {
                return Some((hour, minute));
            }
        }
        let digits = token.trim_end_matches(|character: char| character.is_alphabetic());
        let suffix = &token[digits.len()..];
        if digits.is_empty() {
            continue;
        }
        if let Ok(hour) = digits.parse::<u32>()
            && !suffix.is_empty()
        {
            let hour = apply_meridiem(hour, Some(suffix));
            if hour <= 23 {
                return Some((hour, 0));
            }
        }
    }
    for (index, token) in tokens.iter().enumerate() {
        if !matches!(*token, "at" | "в") {
            continue;
        }
        let Some(number) = tokens.get(index + 1) else {
            continue;
        };
        let digits = number.trim_end_matches(|character: char| character.is_alphabetic());
        let suffix = &number[digits.len()..];
        if let Ok(hour) = digits.parse::<u32>() {
            let marker = if suffix.is_empty() {
                tokens.get(index + 2).copied()
            } else {
                Some(suffix)
            };
            let hour = apply_meridiem(hour, marker);
            if hour <= 23 {
                return Some((hour, 0));
            }
        }
    }
    None
}

fn apply_meridiem(hour: u32, marker: Option<&str>) -> u32 {
    let Some(marker) = marker else {
        return hour;
    };
    let marker = marker.to_lowercase();
    if matches!(marker.as_str(), "pm" | "вечера" | "дня") {
        return if hour >= 12 { hour } else { hour + 12 };
    }
    if matches!(marker.as_str(), "am" | "утра" | "ночи") {
        return if hour == 12 { 0 } else { hour };
    }
    hour
}

fn find_weekday(text: &str) -> Option<u32> {
    const NAMES: &[(&str, u32)] = &[
        ("monday", 1),
        ("mon", 1),
        ("понедельн", 1),
        ("tuesday", 2),
        ("tue", 2),
        ("вторник", 2),
        ("вторн", 2),
        ("wednesday", 3),
        ("wed", 3),
        ("сред", 3),
        ("thursday", 4),
        ("thu", 4),
        ("четверг", 4),
        ("четв", 4),
        ("friday", 5),
        ("fri", 5),
        ("пятниц", 5),
        ("пятн", 5),
        ("saturday", 6),
        ("sat", 6),
        ("суббот", 6),
        ("субб", 6),
        ("sunday", 0),
        ("sun", 0),
        ("воскрес", 0),
    ];
    NAMES
        .iter()
        .find(|(name, _)| text.contains(name))
        .map(|(_, day)| *day)
}

fn find_day_of_month(text: &str) -> Option<u32> {
    for token in text.split_whitespace() {
        let digits = token.trim_end_matches(|character: char| character.is_alphabetic());
        let digits = digits.trim_end_matches('-');
        if let Ok(day) = digits.parse::<u32>()
            && (1..=31).contains(&day)
        {
            return Some(day);
        }
    }
    None
}

/// Best-effort cron description matching `tasks/cron.describe_cron` for the
/// shapes the UI produces; anything else falls back to the raw expression.
fn describe_cron(expression: &str) -> String {
    let fields = expression.split_whitespace().collect::<Vec<_>>();
    let fields = if fields.len() == 6 {
        fields[1..].to_vec()
    } else {
        fields
    };
    if fields.len() != 5 {
        return expression.to_owned();
    }
    let (minute, hour, day, month, weekday) =
        (fields[0], fields[1], fields[2], fields[3], fields[4]);
    let at =
        if minute.chars().all(|c| c.is_ascii_digit()) && hour.chars().all(|c| c.is_ascii_digit()) {
            format!(
                "at {:02}:{:02}",
                hour.parse::<u32>().unwrap_or(0),
                minute.parse::<u32>().unwrap_or(0)
            )
        } else {
            String::new()
        };
    if let Some(step) = minute.strip_prefix("*/")
        && hour == "*"
        && day == "*"
        && month == "*"
        && weekday == "*"
    {
        return format!("every {step} minutes");
    }
    if let Some(step) = hour.strip_prefix("*/")
        && minute.chars().all(|c| c.is_ascii_digit())
        && day == "*"
        && month == "*"
        && weekday == "*"
    {
        return format!(
            "every {step} hours at minute {}",
            minute.parse::<u32>().unwrap_or(0)
        );
    }
    if minute.chars().all(|c| c.is_ascii_digit())
        && hour == "*"
        && day == "*"
        && month == "*"
        && weekday == "*"
    {
        return format!("hourly at minute {}", minute.parse::<u32>().unwrap_or(0));
    }
    let when = if at.is_empty() {
        format!("on cron {expression}")
    } else {
        at
    };
    if weekday != "*" && day == "*" {
        if weekday == "1-5" {
            return format!("every weekday {when}");
        }
        if weekday == "0,6" || weekday == "6,0" {
            return format!("every weekend day {when}");
        }
        let labels = weekday
            .split(',')
            .filter_map(|part| part.parse::<usize>().ok())
            .filter(|index| *index <= 6)
            .map(|index| DOW_LABELS[index])
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            return format!("every {} {when}", labels.join(", "));
        }
    }
    if day.chars().all(|c| c.is_ascii_digit()) && weekday == "*" {
        return format!("monthly on day {} {when}", day.parse::<u32>().unwrap_or(1));
    }
    if day == "*" && weekday == "*" && month == "*" {
        return format!("daily {when}");
    }
    expression.to_owned()
}
