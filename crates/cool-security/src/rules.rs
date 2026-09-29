//! Tool-call policy rules (exec_rules / persistent rules).
//!
//! A [`PolicyRule`] is a first-match-wins decision attached to a tool call
//! subject — a command line (process tools), a workspace-relative path (file
//! tools), or a host name (network tools). Rules are evaluated **before** the
//! capability fallback in the agent loop: a matching rule's decision replaces
//! the capability-derived one; no match falls back to `policy.evaluate`.
//!
//! Scopes mirror the persistence layers: `session` rules live in memory for
//! one run, `project` rules live in `<workspace>/.cool/policy.json`, and
//! `user` rules live in the durable store (`policy_rules` table).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use serde::{Deserialize, Serialize};

use crate::Decision;

/// Where a policy rule is persisted.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    /// In-memory for the current run only.
    #[default]
    Session,
    /// `<workspace>/.cool/policy.json`.
    Project,
    /// Durable store `policy_rules` table (requires the legacy store).
    User,
}

impl RuleScope {
    pub fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Project => "project",
            Self::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "session" => Some(Self::Session),
            "project" => Some(Self::Project),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// What a rule's pattern is matched against.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePatternKind {
    /// Glob on the `"program args…"` command line (`shell`, `git`).
    Command,
    /// Glob on the workspace-relative path (file tools); a leading `!`
    /// negates the match.
    PathGlob,
    /// Host name for network tools; `*` patterns are globs, plain names match
    /// the host or any `*.name` subdomain.
    Domain,
    /// Matches any subject — tool-name-only rules.
    Any,
}

impl RulePatternKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::PathGlob => "path_glob",
            Self::Domain => "domain",
            Self::Any => "any",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "command" => Some(Self::Command),
            "path_glob" => Some(Self::PathGlob),
            "domain" => Some(Self::Domain),
            "any" => Some(Self::Any),
            _ => None,
        }
    }
}

/// The normalized subject a rule is matched against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleSubject {
    /// `"program args…"` for process tools.
    Command(String),
    /// Workspace-relative path for file tools.
    Path(String),
    /// Host name for network tools.
    Domain(String),
    /// No subject — only `Any` patterns apply.
    None,
}

/// One policy rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyRule {
    /// Tool name, or `"*"` for every tool.
    pub tool: String,
    pub kind: RulePatternKind,
    /// The glob/domain text; empty for `Any`.
    #[serde(default)]
    pub pattern: String,
    pub decision: Decision,
    #[serde(default)]
    pub scope: RuleScope,
    #[serde(default)]
    pub note: Option<String>,
    /// Store-assigned identifier (`"user:3"`, `"project:1"`, `"session:0"`)
    /// used by `policy.rule_delete`.
    #[serde(default)]
    pub id: Option<String>,
}

impl PolicyRule {
    pub fn new(
        tool: impl Into<String>,
        kind: RulePatternKind,
        pattern: impl Into<String>,
        decision: Decision,
    ) -> Self {
        Self {
            tool: tool.into(),
            kind,
            pattern: pattern.into(),
            decision,
            scope: RuleScope::Session,
            note: None,
            id: None,
        }
    }

    pub fn scoped(mut self, scope: RuleScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn matches_tool(&self, tool: &str) -> bool {
        self.tool == "*" || self.tool == tool
    }

    /// Whether this rule applies to the call. Only a pattern kind that matches
    /// the subject kind can fire (or `Any`, which ignores the subject).
    pub fn matches(&self, tool: &str, subject: &RuleSubject) -> bool {
        if !self.matches_tool(tool) {
            return false;
        }
        match self.kind {
            RulePatternKind::Any => true,
            RulePatternKind::Command => match subject {
                RuleSubject::Command(line) => glob_match(&self.pattern, line),
                _ => false,
            },
            RulePatternKind::PathGlob => match subject {
                RuleSubject::Path(path) => path_glob_match(&self.pattern, path),
                _ => false,
            },
            RulePatternKind::Domain => match subject {
                RuleSubject::Domain(host) => domain_match(&self.pattern, host),
                _ => false,
            },
        }
    }

    /// Whether another rule has the same `(tool, kind, pattern, decision)`
    /// signature — the dedupe key for persistence layers.
    pub fn same_signature(&self, other: &PolicyRule) -> bool {
        self.tool == other.tool
            && self.kind == other.kind
            && self.pattern == other.pattern
            && self.decision == other.decision
    }

    /// Short human-readable form for the `matched_rule` approval field.
    pub fn describe(&self) -> String {
        let decision = match self.decision {
            Decision::Allow => "allow",
            Decision::Ask => "ask",
            Decision::Deny => "deny",
        };
        if self.pattern.is_empty() {
            format!("{} {} → {}", self.tool, self.kind.name(), decision)
        } else {
            format!(
                "{} {} \"{}\" → {}",
                self.tool,
                self.kind.name(),
                self.pattern,
                decision
            )
        }
    }
}

/// First matching rule in `rules`. First match wins; when several rules share
/// the exact same `(tool, kind, pattern)` signature the strictest decision is
/// returned (`deny > ask > allow` at equal specificity).
pub fn match_rules<'a>(
    rules: impl IntoIterator<Item = &'a PolicyRule>,
    tool: &str,
    subject: &RuleSubject,
) -> Option<&'a PolicyRule> {
    let mut matched: Option<&PolicyRule> = None;
    for rule in rules {
        if !rule.matches(tool, subject) {
            continue;
        }
        match matched {
            None => matched = Some(rule),
            Some(current)
                if rule.tool == current.tool
                    && rule.kind == current.kind
                    && rule.pattern == current.pattern
                    && rule.decision > current.decision =>
            {
                matched = Some(rule);
            }
            Some(_) => {}
        }
    }
    matched
}

/// Shared mutable rule state for one server: the project rules loaded from
/// `<workspace>/.cool/policy.json` plus the live per-run session sets that
/// `approval.resolve {remember: "session"}` and `policy.rule_add` mutate.
/// User-scope rules are not held here — they are read through the durable
/// store (`policy_rules` table) at each merge.
/// One run's session-rule set plus its monotonically increasing id counter
/// (session ids are never reused within a run).
type SessionRuleSet = (Arc<RwLock<Vec<PolicyRule>>>, u64);

#[derive(Default)]
pub struct RuleState {
    project: Mutex<Vec<PolicyRule>>,
    sessions: Mutex<HashMap<String, SessionRuleSet>>,
    /// Rules persisted but not yet committed by their owning approval —
    /// hidden from rule sources until `promote_pending` marks them live.
    /// Shared server-wide so every rule source applies the same window.
    pending: Mutex<Vec<PolicyRule>>,
}

impl RuleState {
    fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
        mutex.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Marks a persisted rule as pending: rule sources skip it until it is
    /// promoted, so a rule never applies before its approval commits.
    pub fn hide_pending(&self, rule: &PolicyRule) {
        Self::lock(&self.pending).push(rule.clone());
    }

    /// Lifts a rule out of the pending window — call once the approval
    /// that backs it has committed (or to drop the marker on abort).
    pub fn promote_pending(&self, rule: &PolicyRule) {
        Self::lock(&self.pending).retain(|pending| !pending.same_signature(rule));
    }

    /// Whether a rule is still inside its pre-commit hidden window.
    pub fn is_pending(&self, rule: &PolicyRule) -> bool {
        Self::lock(&self.pending)
            .iter()
            .any(|pending| pending.same_signature(rule))
    }

    /// The project rules (persisted `project:N` ids, never reused).
    pub fn project_rules(&self) -> Vec<PolicyRule> {
        Self::lock(&self.project).clone()
    }

    pub fn set_project_rules(&self, rules: Vec<PolicyRule>) {
        *Self::lock(&self.project) = rules;
    }

    /// The next never-reused `"{scope}:N"` id — max existing suffix + 1, so
    /// deletions never reassign another rule's id.
    fn next_id(rules: &[PolicyRule], scope: &str) -> String {
        let prefix = format!("{scope}:");
        let next = rules
            .iter()
            .filter_map(|rule| rule.id.as_deref())
            .filter_map(|id| id.strip_prefix(prefix.as_str()))
            .filter_map(|suffix| suffix.parse::<u64>().ok())
            .map(|suffix| suffix + 1)
            .max()
            .unwrap_or(0);
        format!("{scope}:{next}")
    }

    /// Appends a project rule (deduplicated by signature) and returns it with
    /// a stable, never-reused `project:N` id.
    pub fn add_project_rule(&self, rule: PolicyRule) -> PolicyRule {
        let mut rules = Self::lock(&self.project);
        if let Some(existing) = rules.iter().find(|existing| existing.same_signature(&rule)) {
            return existing.clone();
        }
        let mut rule = rule;
        rule.scope = RuleScope::Project;
        rule.id = Some(Self::next_id(&rules, "project"));
        rules.push(rule.clone());
        rule
    }

    /// Deletes the rule whose id is `project:N`. Ids are stable: deleting one
    /// rule never reassigns the ids of the others.
    pub fn delete_project_rule(&self, id: &str) -> bool {
        let mut rules = Self::lock(&self.project);
        let Some(index) = rules.iter().position(|rule| rule.id.as_deref() == Some(id)) else {
            return false;
        };
        rules.remove(index);
        true
    }

    /// The live session-rule set for `run_id`, created on first use.
    pub fn session_rules(&self, run_id: &str) -> Arc<RwLock<Vec<PolicyRule>>> {
        let mut sessions = Self::lock(&self.sessions);
        sessions
            .entry(run_id.to_owned())
            .or_insert_with(|| (Arc::new(RwLock::new(Vec::new())), 0))
            .0
            .clone()
    }

    /// Appends a session rule to `run_id`'s set (deduplicated by signature),
    /// assigning a stable `session:N` id.
    pub fn add_session_rule(&self, run_id: &str, rule: PolicyRule) -> PolicyRule {
        let mut sessions = Self::lock(&self.sessions);
        let (set, counter) = sessions
            .entry(run_id.to_owned())
            .or_insert_with(|| (Arc::new(RwLock::new(Vec::new())), 0));
        let mut rules = set.write().unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = rules.iter().find(|existing| existing.same_signature(&rule)) {
            return existing.clone();
        }
        let mut rule = rule;
        rule.scope = RuleScope::Session;
        rule.id = Some(format!("session:{counter}"));
        *counter += 1;
        rules.push(rule.clone());
        rule
    }

    /// Deletes the rule whose id is `session:N` from `run_id`'s set.
    pub fn delete_session_rule(&self, run_id: &str, id: &str) -> bool {
        let Some((set, _)) = Self::lock(&self.sessions).get(run_id).cloned() else {
            return false;
        };
        let mut rules = set.write().unwrap_or_else(|error| error.into_inner());
        let Some(index) = rules.iter().position(|rule| rule.id.as_deref() == Some(id)) else {
            return false;
        };
        rules.remove(index);
        true
    }

    /// Drops a finished run's session set.
    pub fn remove_session(&self, run_id: &str) {
        Self::lock(&self.sessions).remove(run_id);
    }
}

fn glob_match(pattern: &str, subject: &str) -> bool {
    match globset::Glob::new(pattern) {
        Ok(glob) => glob.compile_matcher().is_match(subject),
        Err(_) => false,
    }
}

/// Path globs additionally support a leading `!` negation (the rule fires when
/// the subject does NOT match the inner glob).
fn path_glob_match(pattern: &str, path: &str) -> bool {
    if let Some(inner) = pattern.strip_prefix('!') {
        return !glob_match(inner, path);
    }
    glob_match(pattern, path)
}

fn domain_match(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    if pattern.contains('*') {
        return glob_match(&pattern, &host);
    }
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(tool: &str, kind: RulePatternKind, pattern: &str, decision: Decision) -> PolicyRule {
        PolicyRule::new(tool, kind, pattern, decision)
    }

    #[test]
    fn first_match_wins_and_ties_take_the_stricter_decision() {
        let rules = [
            mk(
                "shell",
                RulePatternKind::Command,
                "cargo *",
                Decision::Allow,
            ),
            mk("shell", RulePatternKind::Command, "cargo *", Decision::Deny),
            mk("shell", RulePatternKind::Command, "*", Decision::Ask),
        ];
        let subject = RuleSubject::Command("cargo test".to_owned());
        assert_eq!(
            match_rules(&rules, "shell", &subject).unwrap().decision,
            Decision::Deny
        );
        let subject = RuleSubject::Command("npm install".to_owned());
        assert_eq!(
            match_rules(&rules, "shell", &subject).unwrap().decision,
            Decision::Ask
        );
        let subject = RuleSubject::Command("cargo test".to_owned());
        assert!(match_rules(&rules, "git", &subject).is_none());
    }

    #[test]
    fn pattern_kinds_only_match_their_subject_kind() {
        let subject = RuleSubject::Command("rm -rf x".to_owned());
        let rule = mk(
            "shell",
            RulePatternKind::PathGlob,
            "rm -rf x",
            Decision::Deny,
        );
        assert!(!rule.matches("shell", &subject));
        let any = mk("*", RulePatternKind::Any, "", Decision::Deny);
        assert!(any.matches("shell", &subject));
        assert!(any.matches("read_file", &RuleSubject::None));
    }

    #[test]
    fn path_glob_negation() {
        let rule = mk("*", RulePatternKind::PathGlob, "!docs/**", Decision::Deny);
        assert!(!rule.matches("write_file", &RuleSubject::Path("docs/a.md".to_owned())));
        assert!(rule.matches("write_file", &RuleSubject::Path("src/a.rs".to_owned())));
    }

    #[test]
    fn domain_matching_covers_subdomains() {
        let rule = mk("*", RulePatternKind::Domain, "example.com", Decision::Allow);
        assert!(rule.matches(
            "web_fetch",
            &RuleSubject::Domain("api.example.com".to_owned())
        ));
        assert!(rule.matches("web_fetch", &RuleSubject::Domain("example.com".to_owned())));
        assert!(!rule.matches(
            "web_fetch",
            &RuleSubject::Domain("example.com.evil".to_owned())
        ));
        let glob = mk(
            "*",
            RulePatternKind::Domain,
            "*.corp.internal",
            Decision::Deny,
        );
        assert!(glob.matches(
            "web_fetch",
            &RuleSubject::Domain("a.corp.internal".to_owned())
        ));
    }

    #[test]
    fn rule_ids_are_stable_across_deletes_and_deduped_on_re_add() {
        let state = RuleState::default();
        let first = state.add_project_rule(mk(
            "shell",
            RulePatternKind::Command,
            "cargo *",
            Decision::Deny,
        ));
        let second = state.add_project_rule(mk(
            "shell",
            RulePatternKind::Command,
            "npm *",
            Decision::Allow,
        ));
        assert_eq!(first.id.as_deref(), Some("project:0"));
        assert_eq!(second.id.as_deref(), Some("project:1"));

        // Deleting never reassigns ids; re-adding dedupes instead of copying.
        assert!(state.delete_project_rule("project:0"));
        let third = state.add_project_rule(mk(
            "shell",
            RulePatternKind::Command,
            "pip *",
            Decision::Ask,
        ));
        assert_eq!(third.id.as_deref(), Some("project:2"));
        let duplicate = state.add_project_rule(mk(
            "shell",
            RulePatternKind::Command,
            "npm *",
            Decision::Allow,
        ));
        assert_eq!(duplicate.id.as_deref(), Some("project:1"));
        assert_eq!(state.project_rules().len(), 2);
        assert!(state.delete_project_rule("project:1"));
        assert!(!state.delete_project_rule("project:0"));
        assert_eq!(state.project_rules()[0].id.as_deref(), Some("project:2"));

        // Session rules follow the same scheme per run.
        let session = state.add_session_rule(
            "run-1",
            mk("shell", RulePatternKind::Any, "", Decision::Deny),
        );
        assert_eq!(session.id.as_deref(), Some("session:0"));
        assert!(state.delete_session_rule("run-1", "session:0"));
        let next = state.add_session_rule(
            "run-1",
            mk("shell", RulePatternKind::Any, "", Decision::Allow),
        );
        assert_eq!(next.id.as_deref(), Some("session:1"));
    }

    #[test]
    fn round_trip_serialization() {
        let rule = mk("shell", RulePatternKind::Command, "cargo *", Decision::Ask)
            .scoped(RuleScope::Project);
        let json = serde_json::to_string(&rule).unwrap();
        let parsed: PolicyRule = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, rule);
    }
}
