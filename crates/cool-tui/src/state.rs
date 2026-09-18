//! Pure TUI state machine over canonical App Protocol events.
//!
//! The state never touches the durable store or the agent runtime: it consumes
//! the same `ClientState` reducer contract as the TypeScript Web client and
//! emits protocol commands for the client layer to execute.

use std::collections::BTreeMap;

use cool_protocol::{
    ApprovalDecision, ClientState, EventEnvelope, HistoryItem, SessionSummary, StatusGetResult,
    ToolApprovalRequired,
};

use crate::TuiCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEntry {
    User(String),
    Assistant(String),
    Reasoning(String),
    Tool {
        call_id: String,
        name: String,
        status: String,
        summary: Option<String>,
    },
    Notice(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApproval {
    pub approval_id: String,
    pub revision: u64,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMode {
    Chat,
    Sessions,
    Help,
}

/// Terminal-agnostic key input so tests drive the same state machine as the
/// real terminal event loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiKey {
    Char(char),
    Ctrl(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Esc,
    Paste(String),
    None,
}

#[derive(Debug)]
pub struct TuiState {
    pub mode: TuiMode,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub canonical_run_id: Option<String>,
    pub model: Option<String>,
    pub canonical: ClientState,
    pub transcript: Vec<TranscriptEntry>,
    pub input: String,
    pub cursor: usize,
    pub sessions: Vec<SessionSummary>,
    pub session_cursor: usize,
    pub approval: Option<PendingApproval>,
    pub tool_names: BTreeMap<String, String>,
    pub tool_results: BTreeMap<String, String>,
    pub status: StatusGetResult,
    pub notice: Option<String>,
    pub busy: bool,
    pub should_quit: bool,
    pub last_prompt: Option<String>,
    pub auto_scroll: bool,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            mode: TuiMode::Chat,
            session_id: None,
            run_id: None,
            canonical_run_id: None,
            model: None,
            canonical: ClientState::default(),
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            sessions: Vec::new(),
            session_cursor: 0,
            approval: None,
            tool_names: BTreeMap::new(),
            tool_results: BTreeMap::new(),
            status: StatusGetResult::default(),
            notice: None,
            busy: false,
            should_quit: false,
            last_prompt: None,
            auto_scroll: true,
        }
    }
}

impl TuiState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notice(&mut self, message: impl Into<String>) {
        self.notice = Some(message.into());
    }

    pub fn push_transcript(&mut self, entry: TranscriptEntry) {
        self.transcript.push(entry);
    }

    /// Accepts a canonical envelope exactly once, in durable sequence order.
    pub fn on_event(&mut self, envelope: &EventEnvelope) -> Vec<TuiCommand> {
        match &self.canonical_run_id {
            Some(run) if run != &envelope.run_id => {
                if envelope.seq != 1 {
                    // Durable catch-up of an unrelated older run; the current
                    // conversation view keeps its own run state.
                    return Vec::new();
                }
                self.canonical = ClientState::default();
            }
            _ => {}
        }
        if let cool_protocol::CanonicalEvent::ToolRequested(requested) = &envelope.event {
            self.tool_names
                .insert(requested.call_id.clone(), requested.name.clone());
        }
        if let cool_protocol::CanonicalEvent::ToolCompleted(completed) = &envelope.event {
            self.tool_results.insert(
                completed.call_id.clone(),
                summarize(&completed.result.to_string()),
            );
        }
        if let cool_protocol::CanonicalEvent::ToolFailed(failed) = &envelope.event {
            self.tool_results.insert(
                failed.call_id.clone(),
                summarize(failed.message.as_deref().unwrap_or(&failed.error_code)),
            );
        }
        if let Err(error) = self.canonical.try_apply(envelope) {
            self.notice(format!("protocol replay rejected an event: {error}"));
            return Vec::new();
        }
        self.canonical_run_id = Some(envelope.run_id.clone());
        match &envelope.event {
            cool_protocol::CanonicalEvent::RunStarted(_) => {
                self.busy = true;
                self.run_id.get_or_insert_with(|| envelope.run_id.clone());
            }
            cool_protocol::CanonicalEvent::ToolApprovalRequired(approval) => {
                self.set_approval(approval);
            }
            cool_protocol::CanonicalEvent::RunCompleted(_)
            | cool_protocol::CanonicalEvent::RunFailed(_)
            | cool_protocol::CanonicalEvent::RunCancelled(_) => {
                self.finish_run();
            }
            _ => {}
        }
        Vec::new()
    }

    pub fn set_approval(&mut self, approval: &ToolApprovalRequired) {
        self.approval = Some(PendingApproval {
            approval_id: approval.approval_id.clone(),
            revision: approval.revision,
            call_id: approval.call_id.clone(),
            name: approval.name.clone(),
            arguments: serde_json::to_string(&approval.arguments)
                .unwrap_or_else(|_| "{}".to_owned()),
            reason: approval.reason.clone(),
        });
    }

    pub fn finish_run(&mut self) {
        self.busy = false;
        // The canonical client state keeps the run projection intact so it can
        // be compared with the shared reducer contract; the transcript mirrors
        // finalized entries for rendering.
        if !self.canonical.content.is_empty() {
            self.push_transcript(TranscriptEntry::Assistant(self.canonical.content.clone()));
        }
        if !self.canonical.reasoning.is_empty() {
            self.push_transcript(TranscriptEntry::Reasoning(self.canonical.reasoning.clone()));
        }
        for (call_id, status) in &self.canonical.tools {
            let name = self
                .tool_names
                .get(call_id)
                .cloned()
                .unwrap_or_else(|| call_id.clone());
            let summary = self.tool_results.get(call_id).cloned();
            self.transcript.push(TranscriptEntry::Tool {
                call_id: call_id.clone(),
                name,
                status: status.clone(),
                summary,
            });
        }
        self.approval = None;
    }

    pub fn on_key(&mut self, key: TuiKey) -> Vec<TuiCommand> {
        if self.approval.is_some() {
            return self.on_approval_key(key);
        }
        if let TuiKey::Paste(text) = &key {
            self.insert_paste(text);
            return Vec::new();
        }
        match self.mode {
            TuiMode::Sessions => return self.on_sessions_key(key),
            TuiMode::Help => {
                if matches!(key, TuiKey::Esc | TuiKey::Tab | TuiKey::Char('q')) {
                    self.mode = TuiMode::Chat;
                }
                return Vec::new();
            }
            TuiMode::Chat => {}
        }
        match key {
            TuiKey::Ctrl('c' | 'd') | TuiKey::Esc => self.interrupt(),
            TuiKey::Enter | TuiKey::Char('\n') => self.submit(),
            TuiKey::Backspace => {
                if self.cursor > 0 {
                    let index = byte_index(&self.input, self.cursor - 1);
                    self.input.remove(index);
                    self.cursor -= 1;
                }
                Vec::new()
            }
            TuiKey::Delete => {
                if self.cursor < self.input.chars().count() {
                    let index = byte_index(&self.input, self.cursor);
                    self.input.remove(index);
                }
                Vec::new()
            }
            TuiKey::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Vec::new()
            }
            TuiKey::Right => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
                Vec::new()
            }
            TuiKey::Home => {
                self.cursor = 0;
                Vec::new()
            }
            TuiKey::End => {
                self.cursor = self.input.chars().count();
                Vec::new()
            }
            TuiKey::Up | TuiKey::PageUp | TuiKey::Char('\u{1b}') => {
                self.auto_scroll = false;
                Vec::new()
            }
            TuiKey::Down | TuiKey::PageDown => {
                self.auto_scroll = true;
                Vec::new()
            }
            TuiKey::Char(character) => {
                let index = byte_index(&self.input, self.cursor);
                self.input.insert(index, character);
                self.cursor += 1;
                Vec::new()
            }
            TuiKey::Tab => {
                self.mode = TuiMode::Sessions;
                vec![TuiCommand::ListSessions]
            }
            TuiKey::Ctrl(_) | TuiKey::Paste(_) | TuiKey::None => Vec::new(),
        }
    }

    fn on_approval_key(&mut self, key: TuiKey) -> Vec<TuiCommand> {
        let Some(pending) = self.approval.clone() else {
            return Vec::new();
        };
        let decision = match key {
            TuiKey::Char('a' | 'y') => Some(ApprovalDecision::Approved),
            TuiKey::Char('d' | 'n') | TuiKey::Esc | TuiKey::Ctrl('c') => {
                Some(ApprovalDecision::Denied)
            }
            _ => None,
        };
        let Some(decision) = decision else {
            return Vec::new();
        };
        self.approval = None;
        self.push_transcript(TranscriptEntry::Notice(format!(
            "approval {} {}",
            pending.approval_id,
            match decision {
                ApprovalDecision::Approved => "granted",
                ApprovalDecision::Denied => "denied",
            }
        )));
        vec![TuiCommand::ResolveApproval {
            approval_id: pending.approval_id,
            revision: pending.revision,
            decision,
        }]
    }

    fn on_sessions_key(&mut self, key: TuiKey) -> Vec<TuiCommand> {
        match key {
            TuiKey::Esc | TuiKey::Tab => {
                self.mode = TuiMode::Chat;
                Vec::new()
            }
            TuiKey::Up => {
                self.session_cursor = self.session_cursor.saturating_sub(1);
                Vec::new()
            }
            TuiKey::Down => {
                if self.session_cursor + 1 < self.sessions.len() {
                    self.session_cursor += 1;
                }
                Vec::new()
            }
            TuiKey::Enter | TuiKey::Char('\n') => {
                let Some(summary) = self.sessions.get(self.session_cursor) else {
                    return Vec::new();
                };
                let session_id = summary.session_id.clone();
                self.mode = TuiMode::Chat;
                vec![TuiCommand::LoadSession { session_id }]
            }
            TuiKey::Char('f') => {
                let Some(session_id) = self.session_id.clone() else {
                    return Vec::new();
                };
                vec![TuiCommand::ForkSession {
                    session_id,
                    title: None,
                }]
            }
            _ => Vec::new(),
        }
    }

    pub fn insert_paste(&mut self, text: &str) {
        let sanitized = text.replace(['\r', '\n'], " ");
        let index = byte_index(&self.input, self.cursor);
        self.input.insert_str(index, &sanitized);
        self.cursor += sanitized.chars().count();
    }

    fn interrupt(&mut self) -> Vec<TuiCommand> {
        if let (Some(run_id), true) = (self.run_id.clone(), self.busy) {
            self.push_transcript(TranscriptEntry::Notice("cancelling run".to_owned()));
            return vec![TuiCommand::Cancel { run_id }];
        }
        self.should_quit = true;
        Vec::new()
    }

    pub fn submit(&mut self) -> Vec<TuiCommand> {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        if let Some(command) = text.strip_prefix('/') {
            return self.slash_command(command);
        }
        if self.busy {
            let Some(run_id) = self.run_id.clone() else {
                self.notice("no active run to steer");
                return Vec::new();
            };
            self.push_transcript(TranscriptEntry::User(text.clone()));
            return vec![TuiCommand::Steer { run_id, text }];
        }
        let Some(session_id) = self.session_id.clone() else {
            self.notice("no session selected; use /new or press Tab");
            return Vec::new();
        };
        self.auto_scroll = true;
        self.push_transcript(TranscriptEntry::User(text.clone()));
        vec![TuiCommand::Prompt {
            session_id,
            text,
            model: self.model.clone(),
        }]
    }

    pub fn begin_prompt(&mut self, text: String) {
        self.last_prompt = Some(text);
        self.busy = true;
    }

    fn slash_command(&mut self, command: &str) -> Vec<TuiCommand> {
        let (name, argument) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(name, argument)| (name, argument.trim()));
        match name {
            "help" => {
                self.mode = TuiMode::Help;
                Vec::new()
            }
            "quit" | "exit" => {
                self.should_quit = true;
                Vec::new()
            }
            "new" => vec![TuiCommand::CreateSession {
                title: (!argument.is_empty()).then(|| argument.to_owned()),
            }],
            "sessions" => {
                self.mode = TuiMode::Sessions;
                vec![TuiCommand::ListSessions]
            }
            "status" => vec![TuiCommand::RefreshStatus],
            "fork" => {
                let Some(session_id) = self.session_id.clone() else {
                    self.notice("no session to fork");
                    return Vec::new();
                };
                vec![TuiCommand::ForkSession {
                    session_id,
                    title: (!argument.is_empty()).then(|| argument.to_owned()),
                }]
            }
            "cancel" => match (self.run_id.clone(), self.busy) {
                (Some(run_id), true) => vec![TuiCommand::Cancel { run_id }],
                _ => {
                    self.notice("no active run");
                    Vec::new()
                }
            },
            "steer" => match (self.run_id.clone(), self.busy) {
                (Some(run_id), true) if !argument.is_empty() => {
                    self.push_transcript(TranscriptEntry::User(argument.to_owned()));
                    vec![TuiCommand::Steer {
                        run_id,
                        text: argument.to_owned(),
                    }]
                }
                (_, true) => {
                    self.notice("usage: /steer <text>");
                    Vec::new()
                }
                _ => {
                    self.notice("no active run");
                    Vec::new()
                }
            },
            "retry" => {
                let Some(text) = self.last_prompt.clone() else {
                    self.notice("nothing to retry");
                    return Vec::new();
                };
                if self.busy {
                    self.notice("a run is already active");
                    return Vec::new();
                }
                let Some(session_id) = self.session_id.clone() else {
                    self.notice("no session selected");
                    return Vec::new();
                };
                self.busy = true;
                self.push_transcript(TranscriptEntry::User(text.clone()));
                vec![TuiCommand::Prompt {
                    session_id,
                    text,
                    model: self.model.clone(),
                }]
            }
            "model" => {
                if argument.is_empty() {
                    self.notice(match &self.model {
                        Some(model) => format!("model: {model}"),
                        None => "model: server default".to_owned(),
                    });
                } else {
                    self.model = Some(argument.to_owned());
                    self.notice(format!("model set to {argument}"));
                }
                Vec::new()
            }
            other => {
                self.notice(format!("unknown command: /{other}"));
                Vec::new()
            }
        }
    }

    pub fn apply_history(&mut self, items: Vec<HistoryItem>) {
        self.transcript.clear();
        for item in items {
            match item.role.as_str() {
                "user" => self
                    .transcript
                    .push(TranscriptEntry::User(item.content.unwrap_or_default())),
                "assistant" => {
                    if let Some(reasoning) = item.reasoning {
                        self.transcript.push(TranscriptEntry::Reasoning(reasoning));
                    }
                    self.transcript
                        .push(TranscriptEntry::Assistant(item.content.unwrap_or_default()));
                }
                "tool" => self.transcript.push(TranscriptEntry::Tool {
                    call_id: item.tool_call_id.unwrap_or_default(),
                    name: item.name.unwrap_or_else(|| "tool".to_owned()),
                    status: "completed".to_owned(),
                    summary: item.content,
                }),
                _ => {}
            }
        }
    }

    pub fn set_connected_session(&mut self, session_id: String) {
        if self.session_id.as_deref() != Some(session_id.as_str()) {
            self.transcript.clear();
            self.canonical = ClientState::default();
            self.canonical_run_id = None;
            self.run_id = None;
            self.busy = false;
            self.approval = None;
        }
        self.session_id = Some(session_id);
    }

    pub fn status_summary(&self) -> String {
        let plugins = self
            .status
            .plugins
            .iter()
            .map(|entry| format!("{}={}", entry.id, entry.status))
            .collect::<Vec<_>>()
            .join(" ");
        let workers = self
            .status
            .workers
            .iter()
            .map(|entry| format!("{}={}", entry.id, entry.status))
            .collect::<Vec<_>>()
            .join(" ");
        let mcp = self.status.mcp_servers.join(",");
        format!(
            "plugins: {} | workers: {} | mcp: {}",
            if plugins.is_empty() { "-" } else { &plugins },
            if workers.is_empty() { "-" } else { &workers },
            if mcp.is_empty() { "-" } else { &mcp }
        )
    }
}

fn summarize(value: &str) -> String {
    const LIMIT: usize = 120;
    let trimmed = value.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_owned();
    }
    let shortened = trimmed.chars().take(LIMIT).collect::<String>();
    format!("{shortened}…")
}

fn byte_index(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}
