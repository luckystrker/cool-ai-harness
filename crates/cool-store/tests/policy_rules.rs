//! `policy_rules` table round-trip (P1.6, migration version 3).

use cool_store::LegacyStore;
use cool_store::domains::policy_rules::NewPolicyRule;

fn rule(scope: &str, project_key: Option<&str>) -> NewPolicyRule {
    NewPolicyRule {
        tool: "shell".to_owned(),
        pattern_kind: "command".to_owned(),
        pattern: "cargo *".to_owned(),
        decision: "allow".to_owned(),
        scope: scope.to_owned(),
        project_key: project_key.map(str::to_owned),
        note: None,
        created_by: Some("local-user".to_owned()),
    }
}

#[test]
fn policy_rules_insert_list_delete_round_trip() {
    let store = LegacyStore::in_memory().expect("store");

    let inserted = store
        .insert_policy_rule(&rule("user", None))
        .expect("insert");
    assert!(inserted.id > 0);
    assert_eq!(inserted.scope, "user");
    assert_eq!(inserted.pattern, "cargo *");

    // A project rule keyed to another workspace stays invisible.
    store
        .insert_policy_rule(&rule("project", Some("other")))
        .expect("insert project rule");

    let listed = store.list_policy_rules(None).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].scope, "user");

    let keyed = store.list_policy_rules(Some("other")).expect("list keyed");
    assert_eq!(keyed.len(), 2);

    assert!(store.delete_policy_rule(inserted.id).expect("delete"));
    assert!(!store.delete_policy_rule(inserted.id).expect("delete again"));
    let listed = store.list_policy_rules(None).expect("list after delete");
    assert!(listed.is_empty());
}
