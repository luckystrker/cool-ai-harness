//! M10 App Server coverage for the legacy React surface families.
//!
//! Each test drives the real JSON-RPC client against an `AppServer` backed by
//! an in-memory `LegacyStore` (plus the in-memory durable store for M9
//! sessions) and asserts the typed response payloads. Mutations are replayed
//! with the same idempotency key to prove at-most-once behavior.

use std::sync::Arc;

use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::*;
use cool_security::{SecretKey, SecretKeyring};
use cool_state::DurableStore;
use cool_store::LegacyStore;
use cool_store::domains::conversations::{NewConversation, NewMessage};
use cool_store::domains::runs::NewRun;
use cool_store::domains::webhooks::NewWebhookEvent;
use serde_json::json;

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).expect("valid key")
}

fn legacy_server() -> (AppServer, Arc<LegacyStore>) {
    let store = LegacyStore::in_memory().expect("in-memory legacy store");
    store.ensure_actor("local-user").expect("actor");
    let store = Arc::new(store);
    let config = ServerConfig {
        legacy_store: Some(store.clone()),
        ..ServerConfig::default()
    };
    let server =
        AppServer::with_store(config, DurableStore::in_memory().expect("durable")).expect("server");
    (server, store)
}

async fn connected_client(
    server: AppServer,
) -> (AppClient, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client
        .initialize("legacy-surface", "1")
        .await
        .expect("init");
    (client, task)
}

async fn request(client: &AppClient, command: Command) -> ResponsePayload {
    client.request(command).await.expect("command succeeds")
}

#[tokio::test]
async fn legacy_commands_fail_closed_without_a_store() {
    let server = AppServer::new(ServerConfig::default());
    let (client, _task) = connected_client(server).await;
    let error = client
        .request(Command::MemoryStats(EmptyParams {}))
        .await
        .expect_err("must fail closed");
    assert!(matches!(
        error,
        cool_app_server::ClientError::Protocol(protocol) if protocol.cool_code == "legacy_store_unavailable"
    ));
}

#[tokio::test]
async fn conversations_crud_is_idempotent_and_actor_scoped() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let created = request(
        &client,
        Command::ConversationsCreate(ConversationCreateParams {
            idempotency_key: key("conv-1"),
            title: Some("First".to_owned()),
            provider: None,
            model: Some("scripted".to_owned()),
            working_directory: Some("C:/work".to_owned()),
            permissions: None,
            capability_policy: None,
            profile_id: None,
            tags: Some(json!(["a"])),
            folder: Some("inbox".to_owned()),
            metadata: None,
        }),
    )
    .await;
    let ResponsePayload::ConversationsCreated(conversation) = created else {
        panic!("unexpected payload: {created:?}");
    };
    let conversation_id = conversation.id;
    assert_eq!(conversation.title.as_deref(), Some("First"));

    let replayed = request(
        &client,
        Command::ConversationsCreate(ConversationCreateParams {
            idempotency_key: key("conv-1"),
            title: Some("First".to_owned()),
            provider: None,
            model: Some("scripted".to_owned()),
            working_directory: Some("C:/work".to_owned()),
            permissions: None,
            capability_policy: None,
            profile_id: None,
            tags: Some(json!(["a"])),
            folder: Some("inbox".to_owned()),
            metadata: None,
        }),
    )
    .await;
    let ResponsePayload::ConversationsCreated(replayed) = replayed else {
        panic!("unexpected payload");
    };
    assert_eq!(replayed.id, conversation_id);

    let conflict = client
        .request(Command::ConversationsCreate(ConversationCreateParams {
            idempotency_key: key("conv-1"),
            title: Some("Different".to_owned()),
            provider: None,
            model: None,
            working_directory: None,
            permissions: None,
            capability_policy: None,
            profile_id: None,
            tags: None,
            folder: None,
            metadata: None,
        }))
        .await
        .expect_err("conflict expected");
    assert!(matches!(
        conflict,
        cool_app_server::ClientError::Protocol(protocol) if protocol.cool_code == "conflict"
    ));

    let updated = request(
        &client,
        Command::ConversationsUpdate(ConversationUpdateParams {
            idempotency_key: key("conv-update"),
            id: conversation_id,
            title: Some("Renamed".to_owned()),
            provider: None,
            model: None,
            working_directory: None,
            permissions: None,
            capability_policy: None,
            profile_id: None,
            tags: None,
            folder: None,
            is_pinned: Some(true),
            is_archived: None,
            metadata: None,
        }),
    )
    .await;
    assert!(matches!(
        updated,
        ResponsePayload::ConversationsUpdated(conversation)
            if conversation.title.as_deref() == Some("Renamed") && conversation.is_pinned
    ));

    let listed = request(
        &client,
        Command::ConversationsList(ConversationListParams {
            include_machine_owned: false,
            archived: None,
            pinned: Some(true),
            folder: None,
            search: None,
            limit: 10,
            offset: 0,
        }),
    )
    .await;
    let ResponsePayload::ConversationsList(listed) = listed else {
        panic!("unexpected payload");
    };
    assert_eq!(listed.len(), 1);

    let searched = request(
        &client,
        Command::ConversationsSearch(ConversationSearchParams {
            query: "Renamed".to_owned(),
            limit: 5,
        }),
    )
    .await;
    assert!(matches!(
        searched,
        ResponsePayload::ConversationsSearched(rows) if rows.len() == 1
    ));

    store.ensure_actor("other-actor").expect("second actor");
    let other_actor_conversation = store
        .create_conversation(
            "other-actor",
            &NewConversation {
                title: Some("Private".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("other conversation");
    let hidden = client
        .request(Command::ConversationsGet(LegacyIdParams {
            id: other_actor_conversation.id,
        }))
        .await
        .expect_err("hidden from local actor");
    assert!(matches!(
        hidden,
        cool_app_server::ClientError::Protocol(protocol) if protocol.cool_code == "conversation_not_found"
    ));

    let deleted = request(
        &client,
        Command::ConversationsDelete(IdempotentIdParams {
            idempotency_key: key("conv-delete"),
            id: conversation_id,
        }),
    )
    .await;
    assert!(matches!(
        deleted,
        ResponsePayload::ConversationsDeleted(DeletedResult { deleted }) if deleted == conversation_id
    ));
}

#[tokio::test]
async fn conversations_bulk_and_approvals_are_store_backed() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let mut ids = Vec::new();
    for index in 0..3 {
        let conversation = store
            .create_conversation(
                "local-user",
                &NewConversation {
                    title: Some(format!("bulk-{index}")),
                    ..NewConversation::default()
                },
            )
            .expect("conversation");
        ids.push(conversation.id);
    }

    let bulk = request(
        &client,
        Command::ConversationsBulk(ConversationBulkParams {
            idempotency_key: key("bulk-archive"),
            ids: ids.clone(),
            action: "archive".to_owned(),
            folder: None,
        }),
    )
    .await;
    assert!(matches!(
        bulk,
        ResponsePayload::ConversationsBulk(AffectedResult { affected: 3, .. })
    ));

    store
        .add_message(
            "local-user",
            ids[0],
            &NewMessage {
                role: "user".to_owned(),
                content: Some("hello approvals".to_owned()),
                ..NewMessage::default()
            },
        )
        .expect("message");
    store
        .record_approval_audit(
            "local-user",
            ids[0],
            &cool_store::domains::runs::NewApprovalAudit {
                run_id: None,
                call_id: "call-1".to_owned(),
                tool_name: "write_file".to_owned(),
                arguments: None,
                approved: true,
                decision_source: "user".to_owned(),
                decided_by: Some("local-user".to_owned()),
                reason: None,
                is_breakpoint: false,
                breakpoint_type: None,
                duration_ms: Some(5),
            },
        )
        .expect("audit");

    let approvals = request(
        &client,
        Command::ConversationsApprovals(ConversationApprovalsParams {
            id: ids[0],
            run_id: None,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(
        approvals,
        ResponsePayload::ConversationsApprovals(rows) if rows.len() == 1 && rows[0].approved
    ));
}

#[tokio::test]
async fn memory_lifecycle_and_entities_are_protocol_visible() {
    let (server, _store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let created = request(
        &client,
        Command::MemoryCreate(MemoryCreateParams {
            idempotency_key: key("mem-1"),
            scope: None,
            agent_id: None,
            conversation_id: None,
            memory_type: Some("semantic".to_owned()),
            content: "Rust migrations own the legacy store".to_owned(),
            structured: None,
            tags: Some(json!(["rust"])),
            importance: Some(0.9),
            confidence: None,
            source: None,
            status: None,
            confirmed: false,
            supersedes_id: None,
            ttl_days: None,
            valid_from: None,
            valid_to: None,
            pinned: false,
        }),
    )
    .await;
    let ResponsePayload::MemoryCreated(memory) = created else {
        panic!("unexpected payload: {created:?}");
    };
    assert_eq!(memory.status, "active");

    let listed = request(
        &client,
        Command::MemoryList(MemoryListParams {
            memory_type: None,
            scope: None,
            status: None,
            conversation_id: None,
            pinned: None,
            limit: 10,
            offset: 0,
        }),
    )
    .await;
    assert!(matches!(listed, ResponsePayload::MemoryListed(items) if items.len() == 1));

    let pinned = request(
        &client,
        Command::MemoryPin(MemoryPinParams {
            idempotency_key: key("mem-pin"),
            id: memory.id,
            pinned: true,
        }),
    )
    .await;
    assert!(matches!(
        pinned,
        ResponsePayload::MemoryPinned(item) if item.pinned
    ));

    let explained = request(
        &client,
        Command::MemoryExplain(LegacyIdParams { id: memory.id }),
    )
    .await;
    let ResponsePayload::MemoryExplained(explanation) = explained else {
        panic!("unexpected payload");
    };
    assert_eq!(explanation.memory_id, memory.id);
    assert!(explanation.score.total > 0.0);

    let stats = request(&client, Command::MemoryStats(EmptyParams {})).await;
    assert!(matches!(
        stats,
        ResponsePayload::MemoryStats(stats) if stats.total_active == 1
    ));

    let rejected = request(
        &client,
        Command::MemoryReject(IdempotentIdParams {
            idempotency_key: key("mem-reject"),
            id: memory.id,
        }),
    )
    .await;
    assert!(matches!(
        rejected,
        ResponsePayload::MemoryRejected(LegacyOkResult { ok: true })
    ));

    let entity = request(
        &client,
        Command::EntitiesCreate(EntityCreateParams {
            idempotency_key: key("entity-1"),
            name: "Rust".to_owned(),
            entity_type: "technology".to_owned(),
            aliases: None,
            attributes: None,
            description: Some("systems language".to_owned()),
        }),
    )
    .await;
    let ResponsePayload::EntitiesCreated(entity) = entity else {
        panic!("unexpected payload");
    };
    assert_eq!(entity.name, "Rust");

    let entities = request(
        &client,
        Command::EntitiesList(EntityListParams {
            entity_type: None,
            query: Some("Rust".to_owned()),
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(entities, ResponsePayload::EntitiesListed(items) if items.len() == 1));

    request(
        &client,
        Command::EntitiesDelete(IdempotentIdParams {
            idempotency_key: key("entity-delete"),
            id: entity.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn plans_runs_and_inspector_project_the_event_log() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("planned".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let run = store
        .create_run(
            "local-user",
            conversation.id,
            &NewRun {
                model: Some("scripted".to_owned()),
                config: None,
            },
        )
        .expect("run");
    store
        .append_run_event(
            "local-user",
            run.id,
            "llm_call_complete",
            Some(&json!({"model": "scripted", "duration_ms": 25})),
        )
        .expect("event");
    store
        .append_run_event(
            "local-user",
            run.id,
            "finish",
            Some(&json!({"elapsed_ms": 30})),
        )
        .expect("event");
    store
        .add_message(
            "local-user",
            conversation.id,
            &NewMessage {
                role: "user".to_owned(),
                content: Some("do the thing".to_owned()),
                ..NewMessage::default()
            },
        )
        .expect("message");

    let timeline = request(
        &client,
        Command::InspectorTimeline(LegacyIdParams { id: run.id }),
    )
    .await;
    let ResponsePayload::InspectorTimeline(timeline) = timeline else {
        panic!("unexpected payload");
    };
    assert_eq!(timeline.entries.len(), 2);
    assert_eq!(timeline.total_duration_ms, Some(25));

    let compared = request(
        &client,
        Command::InspectorCompare(InspectorCompareParams {
            left_run_id: run.id,
            right_run_id: run.id,
        }),
    )
    .await;
    assert!(matches!(
        compared,
        ResponsePayload::InspectorCompared(comparison) if comparison.deltas.event_count == 0
    ));

    let replayed = request(
        &client,
        Command::InspectorReplay(ReplayParams {
            idempotency_key: key("replay-1"),
            run_id: run.id,
            model: Some("other-model".to_owned()),
            system_prompt: None,
            temperature: Some(0.5),
        }),
    )
    .await;
    let ResponsePayload::InspectorReplayed(replay) = replayed else {
        panic!("unexpected payload");
    };
    assert_eq!(replay.original_run_id, run.id);
    assert_eq!(replay.status, "running");

    let runs = request(
        &client,
        Command::RunsList(RunListParams {
            conversation_id: conversation.id,
            before_id: None,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(runs, ResponsePayload::RunsListed(items) if items.len() == 2));

    let events = request(
        &client,
        Command::RunsEvents(RunEventsLegacyParams {
            run_id: run.id,
            after_seq: None,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(events, ResponsePayload::RunsEvents(items) if items.len() == 2));

    let cancelled = request(
        &client,
        Command::RunsCancel(IdempotentIdParams {
            idempotency_key: key("run-cancel"),
            id: run.id,
        }),
    )
    .await;
    assert!(matches!(
        cancelled,
        ResponsePayload::RunsCancelled(run) if run.status == "cancelled"
    ));

    let plan = store
        .create_plan(
            "local-user",
            conversation.id,
            None,
            Some("Draft"),
            &json!([
                {"position": 1, "title": "Step one", "description": "first step", "status": "pending"}
            ]),
        )
        .expect("plan");

    let listed = request(
        &client,
        Command::PlansList(PlanListParams {
            conversation_id: conversation.id,
        }),
    )
    .await;
    assert!(matches!(listed, ResponsePayload::PlansListed(items) if items.len() == 1));

    let updated = request(
        &client,
        Command::PlansUpdate(PlanUpdateParams {
            idempotency_key: key("plan-update"),
            conversation_id: conversation.id,
            plan_id: plan.id,
            title: Some("Edited".to_owned()),
            steps: Some(json!([
                {"position": 1, "title": "Edited step", "description": "x", "status": "pending"}
            ])),
        }),
    )
    .await;
    assert!(matches!(
        updated,
        ResponsePayload::PlansUpdated(plan) if plan.title.as_deref() == Some("Edited")
    ));

    let approved = request(
        &client,
        Command::PlansApprove(PlanApproveParams {
            idempotency_key: key("plan-approve"),
            conversation_id: conversation.id,
            plan_id: plan.id,
            approved: true,
        }),
    )
    .await;
    assert!(matches!(
        approved,
        ResponsePayload::PlansApproved(plan) if plan.status == "approved"
    ));

    let templates = request(&client, Command::PlansTemplatesList(EmptyParams {})).await;
    assert!(matches!(
        templates,
        ResponsePayload::PlansTemplatesListed(_)
    ));

    let created_template = request(
        &client,
        Command::PlansTemplatesCreate(PlanTemplateCreateParams {
            idempotency_key: key("template-1"),
            name: "review".to_owned(),
            description: None,
            steps: json!([{"position": 1, "title": "Review"}]),
        }),
    )
    .await;
    assert!(matches!(
        created_template,
        ResponsePayload::PlansTemplatesCreated(template) if !template.is_builtin
    ));
}

#[tokio::test]
async fn subagents_roles_launch_and_runs_are_store_backed() {
    let (server, _store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let parent = request(
        &client,
        Command::ConversationsCreate(ConversationCreateParams {
            idempotency_key: key("parent"),
            title: Some("parent".to_owned()),
            provider: None,
            model: None,
            working_directory: None,
            permissions: None,
            capability_policy: None,
            profile_id: None,
            tags: None,
            folder: None,
            metadata: None,
        }),
    )
    .await;
    let ResponsePayload::ConversationsCreated(parent) = parent else {
        panic!("unexpected payload");
    };

    let role = request(
        &client,
        Command::SubagentsRolesCreate(SubagentRoleCreateParams {
            idempotency_key: key("role-1"),
            name: "reviewer".to_owned(),
            description: None,
            system_prompt: Some("review carefully".to_owned()),
            model: None,
            tool_names: Some(json!(["read_file"])),
            capability_policy: None,
            max_iterations: 5,
            max_cost_usd: Some(1.5),
            is_builtin: false,
        }),
    )
    .await;
    let ResponsePayload::SubagentsRolesCreated(role) = role else {
        panic!("unexpected payload");
    };

    let updated = request(
        &client,
        Command::SubagentsRolesUpdate(SubagentRoleUpdateParams {
            idempotency_key: key("role-update"),
            id: role.id,
            name: None,
            description: Some("updated".to_owned()),
            system_prompt: None,
            model: None,
            tool_names: None,
            capability_policy: None,
            max_iterations: None,
            max_cost_usd: None,
            clear_max_cost_usd: true,
        }),
    )
    .await;
    assert!(matches!(
        updated,
        ResponsePayload::SubagentsRolesUpdated(role)
            if role.max_cost_usd.is_none() && role.description.as_deref() == Some("updated")
    ));

    let launched = request(
        &client,
        Command::SubagentsLaunch(SubagentLaunchParams {
            idempotency_key: key("launch-1"),
            parent_conversation_id: parent.id,
            role_id: Some(role.id),
            profile_id: None,
            name: Some("sub one".to_owned()),
            prompt: "check the diff".to_owned(),
            model: Some("scripted".to_owned()),
        }),
    )
    .await;
    let ResponsePayload::SubagentsLaunched(run) = launched else {
        panic!("unexpected payload");
    };
    assert_eq!(run.status, "queued");
    assert!(run.conversation_id > 0);

    let batch = request(
        &client,
        Command::SubagentsLaunchBatch(SubagentLaunchBatchParams {
            idempotency_key: key("launch-batch"),
            parent_conversation_id: parent.id,
            items: vec![
                SubagentLaunchItem {
                    role_id: Some(role.id),
                    profile_id: None,
                    name: Some("batch one".to_owned()),
                    prompt: "first".to_owned(),
                    model: None,
                },
                SubagentLaunchItem {
                    role_id: None,
                    profile_id: None,
                    name: Some("batch two".to_owned()),
                    prompt: "second".to_owned(),
                    model: None,
                },
            ],
        }),
    )
    .await;
    assert!(matches!(
        batch,
        ResponsePayload::SubagentsLaunchedBatch(runs) if runs.len() == 2
    ));

    let runs = request(
        &client,
        Command::SubagentsRunsList(SubagentRunListParams {
            parent_conversation_id: Some(parent.id),
            status: None,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(runs, ResponsePayload::SubagentsRunsListed(items) if items.len() == 3));

    let detail = request(
        &client,
        Command::SubagentsRunsGet(LegacyIdParams { id: run.id }),
    )
    .await;
    assert!(matches!(
        detail,
        ResponsePayload::SubagentsRunsGot(detail) if detail.run.id == run.id
    ));

    let cancelled = request(
        &client,
        Command::SubagentsRunsCancel(IdempotentIdParams {
            idempotency_key: key("sub-cancel"),
            id: run.id,
        }),
    )
    .await;
    assert!(matches!(
        cancelled,
        ResponsePayload::SubagentsRunsCancelled(result)
            if result.run_id == run.id && result.cancelled
    ));

    request(
        &client,
        Command::SubagentsRunsDelete(IdempotentIdParams {
            idempotency_key: key("sub-delete"),
            id: run.id,
        }),
    )
    .await;
    request(
        &client,
        Command::SubagentsRolesDelete(IdempotentIdParams {
            idempotency_key: key("role-delete"),
            id: role.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn research_artifacts_and_constructor_flow_through_the_store() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let research = request(
        &client,
        Command::ResearchCreate(ResearchCreateParams {
            idempotency_key: key("research-1"),
            topic: "Rust store parity".to_owned(),
            depth: 3,
            model: None,
            conversation_id: None,
        }),
    )
    .await;
    let ResponsePayload::ResearchCreated(run) = research else {
        panic!("unexpected payload");
    };
    assert_eq!(run.status, "running");

    let detail = request(&client, Command::ResearchGet(LegacyIdParams { id: run.id })).await;
    assert!(matches!(
        detail,
        ResponsePayload::ResearchGot(detail) if detail.run.id == run.id
    ));

    let rerun = request(
        &client,
        Command::ResearchRerun(ResearchRerunParams {
            idempotency_key: key("research-rerun"),
            id: run.id,
            depth: Some(4),
            model: None,
        }),
    )
    .await;
    assert!(matches!(
        rerun,
        ResponsePayload::ResearchReran(rerun) if rerun.depth == 4
    ));

    let cancelled = request(
        &client,
        Command::ResearchCancel(IdempotentIdParams {
            idempotency_key: key("research-cancel"),
            id: run.id,
        }),
    )
    .await;
    assert!(matches!(
        cancelled,
        ResponsePayload::ResearchCancelled(result) if result.cancelled == run.id
    ));

    let conversation = store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some("artifacts".to_owned()),
                ..NewConversation::default()
            },
        )
        .expect("conversation");
    let artifact = store
        .register_artifact(
            "local-user",
            conversation.id,
            &cool_store::domains::artifacts::NewArtifact {
                filename: "report.md".to_owned(),
                media_type: "text/markdown".to_owned(),
                kind: "document".to_owned(),
                size_bytes: 42,
                sha256: Some("abc".to_owned()),
                storage_path: "sha256/ab/abc".to_owned(),
                tool_call_id: None,
                parent_id: None,
                metadata: None,
                run_id: None,
            },
        )
        .expect("artifact");

    let listed = request(
        &client,
        Command::ArtifactsList(ArtifactListParams {
            conversation_id: conversation.id,
            run_id: None,
            kind: Some("document".to_owned()),
            include_deleted: false,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(listed, ResponsePayload::ArtifactsListed(items) if items.len() == 1));

    let fetched = request(
        &client,
        Command::ArtifactsGet(ArtifactIdParams {
            conversation_id: conversation.id,
            artifact_id: artifact.id,
        }),
    )
    .await;
    assert!(matches!(
        fetched,
        ResponsePayload::ArtifactsGot(detail) if detail.artifact.filename == "report.md"
    ));

    request(
        &client,
        Command::ArtifactsDelete(IdempotentIdParams {
            idempotency_key: key("artifact-delete"),
            id: artifact.id,
        }),
    )
    .await;

    let macro_tool = request(
        &client,
        Command::ConstructorMacrosCreate(MacroCreateParams {
            idempotency_key: key("macro-1"),
            name: "summarize".to_owned(),
            description: "summarize a file".to_owned(),
            input_schema: json!({"type": "object"}),
            steps: json!([{"tool": "read_file"}]),
            is_active: true,
        }),
    )
    .await;
    let ResponsePayload::ConstructorMacrosCreated(macro_tool) = macro_tool else {
        panic!("unexpected payload");
    };

    let macros = request(
        &client,
        Command::ConstructorMacros(ConstructorMacroListParams {
            include_inactive: false,
        }),
    )
    .await;
    assert!(matches!(macros, ResponsePayload::ConstructorMacros(items) if items.len() == 1));

    let updated = request(
        &client,
        Command::ConstructorMacrosUpdate(MacroUpdateParams {
            idempotency_key: key("macro-update"),
            id: macro_tool.id,
            description: Some("updated".to_owned()),
            input_schema: None,
            steps: None,
            is_active: Some(false),
        }),
    )
    .await;
    assert!(matches!(
        updated,
        ResponsePayload::ConstructorMacrosUpdated(tool) if !tool.is_active
    ));

    request(
        &client,
        Command::ConstructorMacrosDelete(IdempotentIdParams {
            idempotency_key: key("macro-delete"),
            id: macro_tool.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn providers_encrypt_secrets_and_expose_cached_models() {
    let store = LegacyStore::in_memory().expect("store");
    store.ensure_actor("local-user").expect("actor");
    let store = Arc::new(store);
    let secret_key = SecretKey::from_secret("test", "unit-test-secret", false).expect("key");
    let config = ServerConfig {
        legacy_store: Some(store.clone()),
        secrets: Some(Arc::new(SecretKeyring::new(secret_key, std::iter::empty()))),
        ..ServerConfig::default()
    };
    let server =
        AppServer::with_store(config, DurableStore::in_memory().expect("durable")).expect("server");
    let (client, _task) = connected_client(server).await;

    let created = request(
        &client,
        Command::ProvidersCreate(ProviderCreateParams {
            idempotency_key: key("provider-1"),
            name: "openai".to_owned(),
            label: Some("OpenAI".to_owned()),
            base_url: Some("https://api.example".to_owned()),
            api_key: Some("sk-plaintext".to_owned()),
            default_model: Some("gpt-4o".to_owned()),
            is_active: true,
            is_subscription: false,
            is_fallback: false,
            is_default: true,
            chat_models: Some(json!([{"id": "gpt-4o", "context_window": 128000}])),
        }),
    )
    .await;
    let ResponsePayload::ProvidersCreated(provider) = created else {
        panic!("unexpected payload");
    };
    assert!(provider.is_default);

    let stored = store
        .get_provider("local-user", provider.id)
        .expect("stored");
    let encrypted = stored.api_key_encrypted.expect("encrypted value");
    assert!(encrypted.contains("ciphertext"));
    assert!(!encrypted.contains("sk-plaintext"));

    let models = request(
        &client,
        Command::ProvidersModels(LegacyIdParams { id: provider.id }),
    )
    .await;
    assert!(matches!(
        models,
        ResponsePayload::ProvidersModels(models)
            if models.len() == 1 && models[0].context_window == Some(128000)
    ));

    let updated = request(
        &client,
        Command::ProvidersUpdate(ProviderUpdateParams {
            idempotency_key: key("provider-update"),
            id: provider.id,
            label: Some("Renamed".to_owned()),
            base_url: None,
            api_key: Some("sk-second".to_owned()),
            default_model: None,
            is_active: Some(false),
            is_fallback: None,
            is_default: None,
            chat_models: None,
        }),
    )
    .await;
    assert!(matches!(
        updated,
        ResponsePayload::ProvidersUpdated(provider) if provider.label.as_deref() == Some("Renamed")
    ));
    let stored = store
        .get_provider("local-user", provider.id)
        .expect("stored");
    assert!(!stored.api_key_encrypted.unwrap().contains("sk-second"));

    request(
        &client,
        Command::ProvidersDelete(IdempotentIdParams {
            idempotency_key: key("provider-delete"),
            id: provider.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn provider_writes_fail_closed_without_a_keyring() {
    let (server, _store) = legacy_server();
    let (client, _task) = connected_client(server).await;
    let error = client
        .request(Command::ProvidersCreate(ProviderCreateParams {
            idempotency_key: key("provider-no-key"),
            name: "openai".to_owned(),
            label: None,
            base_url: None,
            api_key: Some("sk-plaintext".to_owned()),
            default_model: None,
            is_active: true,
            is_subscription: false,
            is_fallback: false,
            is_default: false,
            chat_models: None,
        }))
        .await
        .expect_err("must fail closed");
    assert!(matches!(
        error,
        cool_app_server::ClientError::Protocol(protocol) if protocol.cool_code == "secret_key_unavailable"
    ));
}

#[tokio::test]
async fn budgets_analytics_and_spend_windows_are_protocol_visible() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let updated = request(
        &client,
        Command::BudgetsUpdate(BudgetUpdateParams {
            idempotency_key: key("budget-1"),
            daily_limit_usd: Some(1.0),
            weekly_limit_usd: Some(10.0),
            monthly_limit_usd: Some(50.0),
            alert_threshold_pct: Some(50.0),
            block_on_exceed: Some(true),
        }),
    )
    .await;
    assert!(matches!(
        updated,
        ResponsePayload::BudgetsUpdated(status) if status.status == "ok" && status.daily.limit_usd == Some(1.0)
    ));

    store
        .log_spend(
            "local-user",
            &cool_store::domains::budgets::NewSpendEntry {
                run_id: None,
                conversation_id: None,
                provider_name: "openai".to_owned(),
                model: "gpt-4o".to_owned(),
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                cost_usd: 1.5,
                ts: None,
            },
        )
        .expect("spend");

    let status = request(&client, Command::BudgetsGet(EmptyParams {})).await;
    assert!(matches!(
        status,
        ResponsePayload::BudgetsGot(status) if status.status == "blocked"
    ));

    let until = cool_store::python_datetime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        0,
    );
    let overridden = request(
        &client,
        Command::BudgetsOverrideSet(BudgetOverrideParams {
            idempotency_key: key("budget-override"),
            until,
        }),
    )
    .await;
    assert!(matches!(
        overridden,
        ResponsePayload::BudgetsOverrideSet(status) if status.overridden
    ));

    let cleared = request(
        &client,
        Command::BudgetsOverrideClear(IdempotentParams {
            idempotency_key: key("budget-clear"),
        }),
    )
    .await;
    assert!(matches!(
        cleared,
        ResponsePayload::BudgetsOverrideCleared(status) if !status.overridden
    ));

    let spend = request(
        &client,
        Command::BudgetsSpend(BudgetSpendParams {
            since: None,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(spend, ResponsePayload::BudgetsSpend(rows) if rows.len() == 1));

    let summary = request(
        &client,
        Command::AnalyticsSummary(AnalyticsDaysParams { days: 30 }),
    )
    .await;
    assert!(matches!(
        summary,
        ResponsePayload::AnalyticsSummary(summary) if summary.total_llm_calls == 1
    ));

    let history = request(
        &client,
        Command::AnalyticsCallHistory(AnalyticsCallHistoryParams {
            limit: 10,
            offset: 0,
            model: None,
            provider: None,
        }),
    )
    .await;
    assert!(matches!(
        history,
        ResponsePayload::AnalyticsCallHistory(history) if history.total == 1 && history.rows.len() == 1
    ));

    let spend_over_time = request(
        &client,
        Command::AnalyticsSpendOverTime(AnalyticsBucketParams {
            days: 30,
            bucket: "day".to_owned(),
        }),
    )
    .await;
    assert!(matches!(
        spend_over_time,
        ResponsePayload::AnalyticsSpendOverTime(buckets) if buckets.len() == 1
    ));
}

#[tokio::test]
async fn tasks_scheduler_inbox_and_cron_parse_are_store_backed() {
    let (server, _store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let created = request(
        &client,
        Command::TasksCreate(TaskCreateParams {
            idempotency_key: key("task-1"),
            name: "daily digest".to_owned(),
            description: None,
            trigger_type: "cron".to_owned(),
            cron_expression: Some("0 8 * * *".to_owned()),
            interval_seconds: None,
            run_at: None,
            timezone: "UTC".to_owned(),
            quiet_hours_start: None,
            quiet_hours_end: None,
            misfire_policy: "run".to_owned(),
            prompt: "summarize".to_owned(),
            workflow_type: None,
            profile_id: None,
            model: None,
            tools_whitelist: None,
            capability_policy: None,
            working_directory: None,
            approval_policy: "deny_external".to_owned(),
            delivery_channels: None,
            delivery_config: None,
            max_iterations: 5,
            max_cost_per_run: None,
            timeout_s: None,
            enabled: true,
        }),
    )
    .await;
    let ResponsePayload::TasksCreated(task) = created else {
        panic!("unexpected payload");
    };
    assert!(task.next_run_at.is_some());

    let run = request(
        &client,
        Command::TasksRun(IdempotentIdParams {
            idempotency_key: key("task-run"),
            id: task.id,
        }),
    )
    .await;
    let ResponsePayload::TasksRan(run) = run else {
        panic!("unexpected payload");
    };
    assert_eq!(run.status, "queued");

    let run_detail = request(
        &client,
        Command::TasksRunsGet(LegacyIdParams { id: run.id }),
    )
    .await;
    assert!(matches!(
        run_detail,
        ResponsePayload::TasksRunsGot(detail) if detail.run.id == run.id
    ));

    let read = request(
        &client,
        Command::TasksRunsRead(TaskRunReadParams {
            idempotency_key: key("task-read"),
            id: run.id,
            is_read: true,
        }),
    )
    .await;
    assert!(matches!(read, ResponsePayload::TasksRunsRead(run) if run.is_read));

    let inbox = request(
        &client,
        Command::TasksInbox(TaskInboxParams {
            unread_only: false,
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(
        inbox,
        ResponsePayload::TasksInbox(inbox) if inbox.unread_count == 0 && inbox.runs.len() == 1
    ));

    let scheduler = request(&client, Command::TasksScheduler(EmptyParams {})).await;
    assert!(matches!(
        scheduler,
        ResponsePayload::TasksScheduler(status) if status.jobs.len() == 1
    ));

    let parsed = request(
        &client,
        Command::TasksParseCron(ParseCronParams {
            text: "0 8 * * *".to_owned(),
        }),
    )
    .await;
    assert!(matches!(
        parsed,
        ResponsePayload::TasksParsedCron(parsed)
            if parsed.cron_expression.as_deref() == Some("0 8 * * *") && parsed.next_runs.len() == 3
    ));

    let invalid = request(
        &client,
        Command::TasksParseCron(ParseCronParams {
            text: "not a schedule".to_owned(),
        }),
    )
    .await;
    assert!(matches!(
        invalid,
        ResponsePayload::TasksParsedCron(parsed) if parsed.cron_expression.is_none()
    ));

    for (phrase, expected) in [
        ("every day at 20:30", "30 20 * * *"),
        ("каждый день в 8 вечера", "0 20 * * *"),
        ("every weekday at 9am", "0 9 * * 1-5"),
        ("every 15 minutes", "*/15 * * * *"),
        ("каждый понедельник в 7", "0 7 * * 1"),
    ] {
        let parsed = request(
            &client,
            Command::TasksParseCron(ParseCronParams {
                text: phrase.to_owned(),
            }),
        )
        .await;
        let ResponsePayload::TasksParsedCron(parsed) = parsed else {
            panic!("unexpected payload");
        };
        assert_eq!(
            parsed.cron_expression.as_deref(),
            Some(expected),
            "natural schedule {phrase:?}"
        );
        assert_eq!(parsed.next_runs.len(), 3);
    }

    let cancelled = request(
        &client,
        Command::TasksRunsCancel(IdempotentIdParams {
            idempotency_key: key("task-cancel"),
            id: run.id,
        }),
    )
    .await;
    assert!(matches!(
        cancelled,
        ResponsePayload::TasksRunsCancelled(result) if result.cancelled
    ));

    request(
        &client,
        Command::TasksDelete(IdempotentIdParams {
            idempotency_key: key("task-delete"),
            id: task.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn rss_webhooks_and_wiki_are_protocol_visible() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let subscription = request(
        &client,
        Command::RssSubscribe(RssSubscribeParams {
            idempotency_key: key("rss-1"),
            url: "https://example.com/feed.xml".to_owned(),
            title: None,
            site_url: None,
            category: Some("news".to_owned()),
            fetch_interval_minutes: Some(30),
            enabled: Some(true),
        }),
    )
    .await;
    let ResponsePayload::RssSubscribed(subscription) = subscription else {
        panic!("unexpected payload");
    };
    assert_eq!(subscription.fetch_interval_minutes, 30);

    store
        .insert_entry_if_new(
            "local-user",
            subscription.id,
            &cool_store::domains::rss::NewRssEntry {
                guid: "entry-1".to_owned(),
                title: Some("News".to_owned()),
                link: None,
                author: None,
                summary: None,
                published_at: None,
                content_hash: None,
            },
        )
        .expect("entry");

    let entries = request(
        &client,
        Command::RssEntriesList(RssEntriesParams {
            subscription_id: subscription.id,
            unread_only: true,
            limit: 10,
        }),
    )
    .await;
    let ResponsePayload::RssEntriesListed(entries) = entries else {
        panic!("unexpected payload");
    };
    assert_eq!(entries.len(), 1);

    let read = request(
        &client,
        Command::RssEntryRead(RssEntryReadParams {
            idempotency_key: key("rss-read"),
            id: entries[0].id,
            is_read: true,
        }),
    )
    .await;
    assert!(matches!(read, ResponsePayload::RssEntryRead(entry) if entry.is_read));

    request(
        &client,
        Command::RssUnsubscribe(IdempotentIdParams {
            idempotency_key: key("rss-delete"),
            id: subscription.id,
        }),
    )
    .await;

    let endpoint = request(
        &client,
        Command::WebhooksCreate(WebhookCreateParams {
            idempotency_key: key("webhook-1"),
            name: "github".to_owned(),
            source_type: Some("github".to_owned()),
            event_filter: Some(json!(["push"])),
            task_id: None,
            prompt_template: None,
            enabled: Some(true),
        }),
    )
    .await;
    let ResponsePayload::WebhooksCreated(endpoint) = endpoint else {
        panic!("unexpected payload");
    };
    assert!(!endpoint.hook_id.is_empty());

    let event = store
        .record_webhook_event(
            endpoint.id,
            &NewWebhookEvent {
                event_type: Some("push".to_owned()),
                payload: Some(json!({"ref": "main"})),
                signature_valid: true,
                status: Some("processed"),
            },
        )
        .expect("event");

    let events = request(
        &client,
        Command::WebhooksEvents(WebhookEventsParams {
            endpoint_id: endpoint.id,
            status: Some("processed".to_owned()),
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(events, ResponsePayload::WebhooksEvents(rows) if rows.len() == 1));

    let replayed = request(
        &client,
        Command::WebhooksReplay(WebhookReplayParams {
            idempotency_key: key("webhook-replay"),
            endpoint_id: endpoint.id,
            event_id: event.id,
        }),
    )
    .await;
    assert!(matches!(
        replayed,
        ResponsePayload::WebhooksReplayed(row) if row.id != event.id && row.status == "received"
    ));

    let article = request(
        &client,
        Command::WikiCreate(WikiCreateParams {
            idempotency_key: key("wiki-1"),
            title: "Store parity".to_owned(),
            content: "Rust reads the legacy schema".to_owned(),
            category: Some("dev".to_owned()),
            tags: Some(json!(["rust", "sqlite"])),
            source: None,
            source_memory_id: None,
            project_key: None,
            metadata: None,
        }),
    )
    .await;
    let ResponsePayload::WikiCreated(article) = article else {
        panic!("unexpected payload");
    };

    let promoted = request(
        &client,
        Command::WikiPromote(WikiPromoteParams {
            idempotency_key: key("wiki-promote"),
            memory_item_id: 7,
            title: "From memory".to_owned(),
            content: "content".to_owned(),
            category: None,
            tags: None,
        }),
    )
    .await;
    assert!(matches!(
        promoted,
        ResponsePayload::WikiPromoted(article) if article.source == "memory" && article.source_memory_id == Some(7)
    ));

    let search = request(
        &client,
        Command::WikiSearch(WikiSearchParams {
            query: "legacy schema".to_owned(),
            limit: 10,
        }),
    )
    .await;
    assert!(matches!(search, ResponsePayload::WikiSearched(rows) if rows.len() == 1));

    let tagged = request(
        &client,
        Command::WikiList(WikiListParams {
            category: None,
            tag: Some("sqlite".to_owned()),
            archived: None,
            project_key: None,
            pinned: None,
            search: None,
            limit: 10,
            offset: 0,
        }),
    )
    .await;
    assert!(matches!(tagged, ResponsePayload::WikiListed(rows) if rows.len() == 1));

    let categories = request(&client, Command::WikiCategories(EmptyParams {})).await;
    assert!(matches!(
        categories,
        ResponsePayload::WikiCategories(categories) if categories == vec!["dev".to_owned(), "general".to_owned()]
    ));

    let stats = request(&client, Command::WikiStats(EmptyParams {})).await;
    assert!(matches!(stats, ResponsePayload::WikiStats(stats) if stats.total == 2));

    request(
        &client,
        Command::WikiDelete(IdempotentIdParams {
            idempotency_key: key("wiki-delete"),
            id: article.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn profiles_seed_clone_and_playground_are_store_backed() {
    let (server, _store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let seeded = request(
        &client,
        Command::ProfilesSeed(IdempotentParams {
            idempotency_key: key("seed-1"),
        }),
    )
    .await;
    assert!(matches!(
        seeded,
        ResponsePayload::ProfilesSeeded(SeedResult { created: 5 })
    ));

    let listed = request(
        &client,
        Command::ProfilesList(ProfileListParams {
            include_inactive: false,
        }),
    )
    .await;
    let profiles = match listed {
        ResponsePayload::ProfilesListed(profiles) => profiles,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(profiles.len(), 5);
    let source = profiles.iter().find(|p| p.slug == "coder").unwrap();

    let cloned = request(
        &client,
        Command::ProfilesClone(IdempotentIdParams {
            idempotency_key: key("clone-1"),
            id: source.id,
        }),
    )
    .await;
    let ResponsePayload::ProfilesCloned(cloned) = cloned else {
        panic!("unexpected payload");
    };
    assert_eq!(cloned.slug, "coder-copy");
    assert!(!cloned.is_builtin);

    request(
        &client,
        Command::ProfilesUpdate(ProfileUpdateParams {
            idempotency_key: key("profile-update"),
            id: cloned.id,
            name: Some("Coder Two".to_owned()),
            slug: None,
            description: None,
            system_prompt: None,
            model: Some("scripted".to_owned()),
            tool_names: None,
            skill_names: None,
            settings: None,
            avatar_color: None,
            is_active: Some(true),
            is_shared: None,
        }),
    )
    .await;

    let playground = request(
        &client,
        Command::ProfilesPlayground(ProfilePlaygroundParams {
            idempotency_key: key("playground-1"),
            id: cloned.id,
            title: None,
            initial_prompt: Some("hello".to_owned()),
        }),
    )
    .await;
    assert!(matches!(
        playground,
        ResponsePayload::ProfilesPlayground(PlaygroundResult { conversation_id }) if conversation_id > 0
    ));

    request(
        &client,
        Command::ProfilesDelete(IdempotentIdParams {
            idempotency_key: key("profile-delete"),
            id: cloned.id,
        }),
    )
    .await;
}

#[tokio::test]
async fn workspace_directories_and_git_commands_are_available() {
    let (server, _store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(directory.path().join("alpha")).expect("dir");
    std::fs::create_dir(directory.path().join("beta")).expect("dir");
    std::fs::create_dir(directory.path().join(".hidden")).expect("dir");

    let listing = request(
        &client,
        Command::WorkspaceDirectories(WorkspaceOptionalPathParams {
            path: Some(directory.path().to_string_lossy().into_owned()),
        }),
    )
    .await;
    let ResponsePayload::WorkspaceDirectories(listing) = listing else {
        panic!("unexpected payload");
    };
    assert_eq!(listing.directories, vec!["alpha", "beta"]);
    assert!(listing.parent.is_some());

    request(&client, Command::WorkspaceRecent(EmptyParams {})).await;

    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok()
    {
        let repo = std::env::current_dir().expect("cwd");
        let info = request(
            &client,
            Command::WorkspaceGitInfo(WorkspacePathParams {
                path: repo.to_string_lossy().into_owned(),
            }),
        )
        .await;
        assert!(matches!(
            info,
            ResponsePayload::WorkspaceGitInfo(info) if info.is_git
        ));

        let status = request(
            &client,
            Command::WorkspaceGitStatus(WorkspacePathParams {
                path: repo.to_string_lossy().into_owned(),
            }),
        )
        .await;
        assert!(matches!(
            status,
            ResponsePayload::WorkspaceGitStatus(status) if status.is_git
        ));

        let scratch = tempfile::tempdir().expect("scratch repo");
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(scratch.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        run_git(&["init", "-q"]);
        run_git(&["config", "user.email", "tests@example.com"]);
        run_git(&["config", "user.name", "Tests"]);
        std::fs::write(scratch.path().join("file.txt"), "hello").expect("write");
        run_git(&["add", "file.txt"]);
        run_git(&["commit", "-q", "-m", "init"]);
        run_git(&["branch", "feature"]);
        let scratch_path = scratch.path().to_string_lossy().into_owned();

        let checkout = request(
            &client,
            Command::WorkspaceGitCheckout(WorkspaceGitCheckoutParams {
                idempotency_key: key("checkout-1"),
                path: scratch_path.clone(),
                branch: "feature".to_owned(),
            }),
        )
        .await;
        assert!(matches!(
            checkout,
            ResponsePayload::WorkspaceGitCheckout(result)
                if result.branch == "feature" && result.status == "checked_out"
        ));
        let replay = request(
            &client,
            Command::WorkspaceGitCheckout(WorkspaceGitCheckoutParams {
                idempotency_key: key("checkout-1"),
                path: scratch_path.clone(),
                branch: "feature".to_owned(),
            }),
        )
        .await;
        assert!(matches!(
            replay,
            ResponsePayload::WorkspaceGitCheckout(result) if result.branch == "feature"
        ));
        let branches = request(
            &client,
            Command::WorkspaceGitBranches(WorkspacePathParams { path: scratch_path }),
        )
        .await;
        assert!(matches!(
            branches,
            ResponsePayload::WorkspaceGitBranches(branches)
                if branches.current.as_deref() == Some("feature")
        ));
    }
}

#[tokio::test]
async fn legacy_idempotency_replay_does_not_repeat_mutations() {
    let (server, store) = legacy_server();
    let (client, _task) = connected_client(server).await;

    for _ in 0..2 {
        request(
            &client,
            Command::MemoryCreate(MemoryCreateParams {
                idempotency_key: key("replayed"),
                scope: None,
                agent_id: None,
                conversation_id: None,
                memory_type: None,
                content: "once".to_owned(),
                structured: None,
                tags: None,
                importance: None,
                confidence: None,
                source: None,
                status: None,
                confirmed: false,
                supersedes_id: None,
                ttl_days: None,
                valid_from: None,
                valid_to: None,
                pinned: false,
            }),
        )
        .await;
    }
    let filter = cool_store::domains::memory::MemoryFilter {
        limit: Some(10),
        ..Default::default()
    };
    let rows = store
        .list_memory_items("local-user", &filter)
        .expect("rows");
    assert_eq!(rows.len(), 1, "replay must not create a second memory");
}
