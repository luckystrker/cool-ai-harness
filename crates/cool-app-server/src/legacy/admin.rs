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
use cool_store::domains::tasks::{NewScheduledTask, ScheduledTask, ScheduledTaskPatch};
use cool_store::domains::webhooks::{NewWebhookEndpoint, WebhookEndpointPatch};
use cool_store::{LegacyStore, StoreError};
use serde_json::Value;

use super::{
    Unhandled, bridge, convert, encrypt_secret, fingerprint, idempotent, invalid_input, store_error,
};

const DEFAULT_SOURCE_TYPE: &str = "generic";

pub(super) async fn dispatch(
    store: &LegacyStore,
    secrets: Option<&SecretKeyring>,
    _workspace_root: &Path,
    actor: &ActorRef,
    command: Command,
) -> Result<ResponsePayload, Unhandled> {
    let payload = match command {
        Command::ProvidersList(params) => ResponsePayload::ProvidersListed(
            store
                .list_providers(&actor.id, params.include_inactive)
                .map_err(store_error)?
                .into_iter()
                .map(|provider| provider_record(provider, secrets))
                .collect::<Result<Vec<_>, _>>()?,
        ),
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
            ResponsePayload::ProvidersCreated(provider_record(created, secrets)?)
        }
        Command::ProvidersGet(params) => ResponsePayload::ProvidersGot(provider_record(
            store
                .get_provider(&actor.id, params.id)
                .map_err(store_error)?,
            secrets,
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
            ResponsePayload::ProvidersUpdated(provider_record(updated, secrets)?)
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
            ResponsePayload::TasksListed(
                tasks
                    .into_iter()
                    .map(task_record)
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        Command::TasksGet(params) => ResponsePayload::TasksGot(task_record(
            store.get_task(&actor.id, params.id).map_err(store_error)?,
        )?),
        Command::TasksCreate(params) => {
            let new = task_create_payload(&params)?;
            let created = idempotent(
                store,
                actor,
                "tasks.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_task(&actor.id, &new),
            )?;
            ResponsePayload::TasksCreated(task_record(created)?)
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
            ResponsePayload::TasksUpdated(task_record(updated)?)
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

/// Convert a provider row and attach the masked key hint (never the key).
fn provider_record(
    provider: Provider,
    secrets: Option<&SecretKeyring>,
) -> Result<ProviderRecord, ProtocolError> {
    let hint = provider.api_key_encrypted.as_deref().map(|stored| {
        match secrets.and_then(|keyring| keyring.decrypt(stored).ok()) {
            Some(plaintext) => mask_secret(&plaintext),
            None => "<undecryptable>".to_owned(),
        }
    });
    let mut record: ProviderRecord = convert(provider)?;
    record.api_key_hint = hint;
    Ok(record)
}

/// Port of the Python `_mask`: `abc…wxyz` for long secrets, `…` otherwise.
fn mask_secret(secret: &str) -> String {
    let characters = secret.chars().collect::<Vec<_>>();
    if characters.len() <= 8 {
        return "…".to_owned();
    }
    let head = characters[..3].iter().collect::<String>();
    let tail = characters[characters.len() - 4..]
        .iter()
        .collect::<String>();
    format!("{head}…{tail}")
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

fn task_record(task: ScheduledTask) -> Result<TaskRecord, ProtocolError> {
    let mut record: TaskRecord = convert(task)?;
    // Match Python's `next_cron_runs(..., timezone=task.timezone)`: an unknown
    // zone falls back to UTC, exactly like `resolve_timezone`.
    let offset = cool_store::scheduler::timezone_offset_seconds(&record.timezone).unwrap_or(0);
    if let Some(expression) = record.cron_expression.clone()
        && let Ok(runs) =
            cool_store::scheduler::cron_next_runs_at(&expression, offset, super::now_seconds(), 3)
    {
        record.schedule_description = Some(cool_store::scheduler::describe_cron(&expression));
        record.next_runs = runs.into_iter().map(format_run_time).collect();
    }
    Ok(record)
}

fn format_run_time(timestamp: i64) -> String {
    format!(
        "{}Z",
        cool_store::python_datetime(timestamp, 0).replace(' ', "T")
    )
}

/// Build the store create payload, expanding an optional built-in template the
/// same way the Python endpoint does: the template supplies prompt, cron,
/// workflow type, tool whitelist and delivery channels only where the caller
/// left them unset.
fn task_create_payload(params: &TaskCreateParams) -> Result<NewScheduledTask, ProtocolError> {
    let mut new: NewScheduledTask = bridge(params)?;
    if let Some(slug) = params.template.as_deref() {
        let preset = crate::task_templates()
            .into_iter()
            .find(|template| template.slug == slug)
            .ok_or_else(|| invalid_input(format!("unknown template {slug:?}")))?;
        if new.prompt.is_empty() {
            new.prompt = preset.prompt;
        }
        // Python uses `body.x or preset.x`, so an empty string/list also falls
        // back to the preset (not just a missing value).
        if new.cron_expression.as_deref().unwrap_or("").is_empty() {
            new.cron_expression = Some(preset.cron_expression);
        }
        if new.workflow_type.is_none() {
            new.workflow_type = Some(preset.slug);
        }
        if json_value_is_empty(new.tools_whitelist.as_ref()) {
            new.tools_whitelist = preset
                .tools_whitelist
                .map(|tools| Value::Array(tools.into_iter().map(Value::String).collect()));
        }
        if json_value_is_empty(new.delivery_channels.as_ref()) {
            new.delivery_channels = Some(Value::Array(
                preset
                    .delivery_channels
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ));
        }
    }
    Ok(new)
}

fn json_value_is_empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::String(text)) => text.is_empty(),
        Some(_) => false,
    }
}

fn parse_cron(text: &str) -> ParseCronResult {
    let text = text.trim();
    let expression = if text.is_empty() {
        None
    } else if cool_store::scheduler::cron_next_runs(text, super::now_seconds(), 1).is_ok() {
        Some(text.to_owned())
    } else {
        cool_store::scheduler::parse_natural_schedule(text)
    };
    match expression {
        Some(expression) => {
            match cool_store::scheduler::cron_next_runs(&expression, super::now_seconds(), 3) {
                Ok(runs) => ParseCronResult {
                    cron_expression: Some(expression.clone()),
                    description: Some(cool_store::scheduler::describe_cron(&expression)),
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
