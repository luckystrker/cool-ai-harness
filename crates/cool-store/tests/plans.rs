//! Plan, plan-step and plan-template store parity tests (Фаза 2 §1).

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::{LegacyStore, StoreError};
use serde_json::json;
use tempfile::TempDir;

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let store = LegacyStore::open(
        &path,
        &cool_store::StoreOptions {
            initialize_if_missing: false,
            ..cool_store::StoreOptions::default()
        },
    )
    .expect("adopt");
    (directory, store)
}

#[test]
fn plan_creation_persists_ordered_step_rows() {
    let (_directory, store) = adopted_store();
    let plan = store
        .create_plan(
            "local-user",
            1,
            None,
            Some("Ship it"),
            &json!([
                {
                    "position": 0,
                    "title": "Research",
                    "description": "Read the docs",
                    "depends_on": [],
                    "tools": ["read_file"],
                    "delegate_role": "researcher"
                },
                {"position": 1, "title": "Implement", "depends_on": [0]}
            ]),
        )
        .expect("create plan");
    assert_eq!(plan.status, "draft");
    assert_eq!(plan.title.as_deref(), Some("Ship it"));

    let steps = store.list_plan_steps(plan.id).expect("steps");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].position, 0);
    assert_eq!(steps[0].title, "Research");
    assert_eq!(steps[0].status, "pending");
    assert_eq!(steps[0].tools, Some(json!(["read_file"])));
    assert_eq!(steps[0].delegate_role.as_deref(), Some("researcher"));
    assert_eq!(steps[1].depends_on, Some(json!([0])));
    assert_eq!(steps[1].position, 1);

    let listed = store.list_plans("local-user", 1).expect("list plans");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, plan.id);
}

#[test]
fn plan_step_sequencing_and_draft_only_edits() {
    let (_directory, store) = adopted_store();
    let plan = store
        .create_plan(
            "local-user",
            1,
            None,
            Some("Plan"),
            &json!([{"title": "One"}, {"title": "Two"}]),
        )
        .expect("create");

    let steps = store.list_plan_steps(plan.id).expect("steps");
    assert_eq!(
        steps.iter().map(|step| step.position).collect::<Vec<_>>(),
        vec![0, 1],
        "missing positions fall back to the array index"
    );

    let first = store
        .update_plan_step_status(plan.id, 0, "running", None, None)
        .expect("running");
    assert_eq!(first.status, "running");
    let first = store
        .update_plan_step_status(plan.id, 0, "completed", Some("done"), None)
        .expect("completed");
    assert_eq!(first.status, "completed");
    assert_eq!(first.result_summary.as_deref(), Some("done"));

    let missing = store
        .update_plan_step_status(plan.id, 9, "completed", None, None)
        .expect_err("missing step");
    assert!(matches!(missing, StoreError::NotFound("plan step")));
    let bad_status = store
        .update_plan_step_status(plan.id, 1, "bogus", None, None)
        .expect_err("bad status");
    assert!(matches!(bad_status, StoreError::InvalidInput(_)));

    let replaced = store
        .replace_plan_steps(
            "local-user",
            1,
            plan.id,
            &json!([{"title": "Only"}, {"title": "Two"}]),
        )
        .expect("replace");
    assert_eq!(replaced.status, "draft");
    let steps = store.list_plan_steps(plan.id).expect("steps after replace");
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.position, step.status.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "pending"), (1, "pending")]
    );

    let err = store
        .set_plan_status("local-user", 1, plan.id, "nonsense")
        .expect_err("bad plan status");
    assert!(matches!(err, StoreError::InvalidInput(_)));

    let approved = store
        .set_plan_status("local-user", 1, plan.id, "approved")
        .expect("approve");
    assert_eq!(approved.status, "approved");

    let rejected = store
        .replace_plan_steps("local-user", 1, plan.id, &json!([{"title": "X"}]))
        .expect_err("approved plan is no longer editable");
    assert!(matches!(rejected, StoreError::InvalidInput(_)));
}

#[test]
fn plans_are_actor_scoped_and_deletable() {
    let (_directory, store) = adopted_store();
    let plan = store
        .create_plan("local-user", 1, None, None, &json!([{"title": "One"}]))
        .expect("create");

    store.ensure_actor("other-actor").expect("register actor");
    let error = store
        .get_plan("other-actor", 1, plan.id)
        .expect_err("cross actor");
    assert!(matches!(error, StoreError::NotFound("conversation")));

    let second = store
        .create_plan(
            "local-user",
            1,
            None,
            Some("Newer"),
            &json!([{"title": "Two"}]),
        )
        .expect("second plan");
    let listed = store.list_plans("local-user", 1).expect("list");
    assert_eq!(listed[0].id, second.id, "newest plan first");

    store.delete_plan("local-user", 1, plan.id).expect("delete");
    assert!(store.list_plan_steps(plan.id).expect("steps").is_empty());
    let gone = store.get_plan("local-user", 1, plan.id).expect_err("gone");
    assert!(matches!(gone, StoreError::NotFound("plan")));
}

#[test]
fn plan_templates_round_trip_and_protect_builtins() {
    let (_directory, store) = adopted_store();
    let template = store
        .create_plan_template(
            "Research",
            Some("A research skeleton"),
            &json!([{"title": "Gather sources"}]),
        )
        .expect("create template");
    assert!(!template.is_builtin);
    assert_eq!(template.steps, json!([{"title": "Gather sources"}]));

    let templates = store.list_plan_templates().expect("list");
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].id, template.id);

    store.with_connection(|connection| {
        connection
            .execute(
                "INSERT INTO plan_templates(created_at, updated_at, name, description, steps, is_builtin)
                 VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 'Built-in', NULL, '[]', 1)",
                [],
            )
            .expect("insert builtin");
        Ok(())
    })
    .expect("seed builtin");

    let builtin = store
        .list_plan_templates()
        .expect("list")
        .into_iter()
        .find(|row| row.is_builtin)
        .expect("builtin present");
    let protected = store
        .delete_plan_template(builtin.id)
        .expect_err("builtin protected");
    assert!(matches!(protected, StoreError::InvalidInput(_)));

    store
        .delete_plan_template(template.id)
        .expect("delete template");
    let gone = store
        .delete_plan_template(template.id)
        .expect_err("missing template");
    assert!(matches!(gone, StoreError::NotFound("plan template")));
}
