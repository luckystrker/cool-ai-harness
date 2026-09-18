//! Projection from canonical Cool App Protocol events to ACP v1 session updates.
//!
//! This mirrors the M4 Python adapter contract: ACP consumes canonical
//! envelopes, never executor callbacks, so Web/ACP/TUI share one event stream.

use std::collections::BTreeMap;

use cool_protocol::{CanonicalEvent, EventEnvelope};
use serde_json::{Value, json};

/// Stateful ACP projection for one prompt turn.
#[derive(Default)]
pub struct AcpProjection {
    known_tools: BTreeMap<String, String>,
    plan_entries: Vec<Value>,
    saw_content_delta: bool,
}

impl AcpProjection {
    pub fn adapt(&mut self, envelope: &EventEnvelope) -> Vec<Value> {
        match &envelope.event {
            CanonicalEvent::ContentDelta(delta) => {
                self.saw_content_delta = true;
                vec![message_chunk(&delta.text, "agent")]
            }
            CanonicalEvent::ReasoningDelta(delta) => {
                vec![thought_chunk(&delta.text)]
            }
            CanonicalEvent::ToolRequested(requested) => {
                if self.known_tools.contains_key(&requested.call_id) {
                    return vec![json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": requested.call_id,
                        "rawInput": requested.arguments,
                    })];
                }
                self.known_tools
                    .insert(requested.call_id.clone(), requested.name.clone());
                vec![json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": requested.call_id,
                    "title": requested.name,
                    "kind": tool_kind(&requested.name),
                    "status": "pending",
                    "rawInput": requested.arguments,
                })]
            }
            CanonicalEvent::ToolApprovalRequired(approval) => {
                vec![tool_update(&approval.call_id, "pending", None)]
            }
            CanonicalEvent::ToolApprovalResolved(resolved) => vec![tool_update(
                &resolved.call_id,
                "in_progress",
                Some(json!({"permissionDecision": decision_name(&resolved.decision)})),
            )],
            CanonicalEvent::ToolCompleted(completed) => vec![tool_update(
                &completed.call_id,
                "completed",
                Some(completed.result.clone()),
            )],
            CanonicalEvent::ToolFailed(failed) => vec![tool_update(
                &failed.call_id,
                "failed",
                Some(json!({
                    "errorCode": failed.error_code,
                    "message": failed.message,
                })),
            )],
            CanonicalEvent::ItemCompleted(item)
                if !self.saw_content_delta && item.role.as_deref() == Some("assistant") =>
            {
                match item.content.as_deref() {
                    Some(content) if !content.is_empty() => vec![message_chunk(content, "agent")],
                    _ => Vec::new(),
                }
            }
            // Canonical usage is per run/call while ACP v1 usage represents the
            // current session context. Publishing an invented sum stays omitted.
            CanonicalEvent::UsageUpdated(_) => Vec::new(),
            event @ (CanonicalEvent::PlanCreated(_)
            | CanonicalEvent::PlanStepStarted(_)
            | CanonicalEvent::PlanStepCompleted(_)
            | CanonicalEvent::PlanProgress(_)) => self.adapt_plan(event),
            _ => Vec::new(),
        }
    }

    pub fn permission_tool_call(&self, call_id: &str, name: &str, arguments: &Value) -> Value {
        let name = if name.is_empty() {
            self.known_tools
                .get(call_id)
                .cloned()
                .unwrap_or_else(|| "Tool".to_owned())
        } else {
            name.to_owned()
        };
        json!({
            "toolCallId": call_id,
            "title": name,
            "kind": tool_kind(&name),
            "status": "pending",
            "rawInput": arguments,
        })
    }

    fn adapt_plan(&mut self, event: &CanonicalEvent) -> Vec<Value> {
        match event {
            CanonicalEvent::PlanCreated(created) => {
                self.plan_entries.clear();
                for index in 0..created.total_steps {
                    let title = created
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("Step {}", index + 1));
                    self.plan_entries.push(json!({
                        "content": title,
                        "priority": "medium",
                        "status": "pending",
                    }));
                }
            }
            CanonicalEvent::PlanStepStarted(step) | CanonicalEvent::PlanStepCompleted(step) => {
                let completed = matches!(event, CanonicalEvent::PlanStepCompleted(_));
                let index = step.position.saturating_sub(1) as usize;
                while self.plan_entries.len() <= index {
                    self.plan_entries.push(json!({
                        "content": format!("Step {}", self.plan_entries.len() + 1),
                        "priority": "medium",
                        "status": "pending",
                    }));
                }
                if let Some(entry) = self.plan_entries.get_mut(index)
                    && let Some(object) = entry.as_object_mut()
                {
                    if !step.title.is_empty() {
                        object.insert("content".to_owned(), json!(step.title));
                    }
                    object.insert(
                        "status".to_owned(),
                        json!(if completed {
                            "completed"
                        } else {
                            "in_progress"
                        }),
                    );
                }
            }
            CanonicalEvent::PlanProgress(progress) => {
                if matches!(
                    progress.status,
                    cool_protocol::PlanProgressStatus::Completed
                ) {
                    for entry in &mut self.plan_entries {
                        if let Some(object) = entry.as_object_mut() {
                            object.insert("status".to_owned(), json!("completed"));
                        }
                    }
                }
            }
            _ => {}
        }
        if self.plan_entries.is_empty() {
            return Vec::new();
        }
        vec![json!({
            "sessionUpdate": "plan",
            "entries": self.plan_entries,
        })]
    }
}

pub fn message_chunk(text: &str, role: &str) -> Value {
    json!({
        "sessionUpdate": format!("{role}_message_chunk"),
        "content": {"type": "text", "text": text},
    })
}

pub fn thought_chunk(text: &str) -> Value {
    json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": {"type": "text", "text": text},
    })
}

pub fn tool_kind(name: &str) -> &'static str {
    let lowered = name.to_ascii_lowercase();
    let has = |parts: &[&str]| parts.iter().any(|part| lowered.contains(part));
    if has(&["delete", "remove"]) {
        "delete"
    } else if has(&["move", "rename"]) {
        "move"
    } else if has(&["write", "edit", "patch", "create_file"]) {
        "edit"
    } else if has(&["read", "list", "glob"]) {
        "read"
    } else if has(&["search", "grep", "find"]) {
        "search"
    } else if has(&["bash", "shell", "execute", "python", "terminal"]) {
        "execute"
    } else if has(&["http", "web", "fetch", "rss"]) {
        "fetch"
    } else if lowered.contains("plan") {
        "think"
    } else {
        "other"
    }
}

fn tool_update(call_id: &str, status: &str, raw_output: Option<Value>) -> Value {
    let mut update = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": call_id,
        "status": status,
    });
    if let (Some(raw_output), Some(object)) = (raw_output, update.as_object_mut()) {
        object.insert("rawOutput".to_owned(), raw_output);
    }
    update
}

fn decision_name(decision: &cool_protocol::ApprovalOutcome) -> &'static str {
    match decision {
        cool_protocol::ApprovalOutcome::Approved => "approved",
        cool_protocol::ApprovalOutcome::Denied => "denied",
        cool_protocol::ApprovalOutcome::TimedOut => "timed_out",
    }
}
