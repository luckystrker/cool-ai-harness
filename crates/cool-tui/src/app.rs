//! Event loop and command execution for the Cool TUI.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cool_app_server::AppClient;
use cool_app_server::client::new_idempotency_key;
use cool_protocol::{ApprovalDecision, EventEnvelope};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::broadcast;

use crate::state::{TuiKey, TuiMode, TuiState};
use crate::ui;

#[derive(Clone, Debug, PartialEq)]
pub enum TuiCommand {
    Prompt {
        session_id: String,
        text: String,
        model: Option<String>,
    },
    Steer {
        run_id: String,
        text: String,
    },
    Cancel {
        run_id: String,
    },
    ResolveApproval {
        approval_id: String,
        revision: u64,
        decision: ApprovalDecision,
    },
    LoadSession {
        session_id: String,
    },
    ForkSession {
        session_id: String,
        title: Option<String>,
    },
    CreateSession {
        title: Option<String>,
    },
    ListSessions,
    RefreshStatus,
}

#[derive(Clone, Debug)]
pub enum TuiEvent {
    Key(TuiKey),
    Resize(u16, u16),
    Server(Box<EventEnvelope>),
    Tick,
    Disconnected(String),
}

pub struct TuiApp<B: Backend> {
    pub terminal: Terminal<B>,
    pub state: TuiState,
    pub client: AppClient,
    events: broadcast::Receiver<EventEnvelope>,
    command_counter: Arc<AtomicU64>,
    pub width: u16,
    pub height: u16,
}

impl<B: Backend> TuiApp<B> {
    pub fn new(terminal: Terminal<B>, client: AppClient, width: u16, height: u16) -> Self {
        let events = client.subscribe();
        Self {
            terminal,
            state: TuiState::new(),
            client,
            events,
            command_counter: Arc::new(AtomicU64::new(1)),
            width,
            height,
        }
    }

    pub fn event_receiver(&self) -> broadcast::Receiver<EventEnvelope> {
        self.events.resubscribe()
    }

    fn idempotency_key(&self, prefix: &str) -> String {
        let counter = self.command_counter.fetch_add(1, Ordering::SeqCst);
        new_idempotency_key(&format!("tui-{prefix}-{counter}"))
    }

    pub fn draw(&mut self) -> io::Result<()> {
        self.terminal.draw(|frame| ui::render(frame, &self.state))?;
        Ok(())
    }

    pub async fn handle(&mut self, event: TuiEvent) -> io::Result<()> {
        match event {
            TuiEvent::Key(key) => {
                let commands = self.state.on_key(key);
                self.execute(commands).await;
            }
            TuiEvent::Resize(width, height) => {
                self.width = width;
                self.height = height;
            }
            TuiEvent::Server(envelope) => {
                let commands = self.state.on_event(&envelope);
                self.execute(commands).await;
            }
            TuiEvent::Disconnected(message) => {
                self.state
                    .notice(format!("server connection lost: {message}"));
                self.state.busy = false;
            }
            TuiEvent::Tick => {}
        }
        self.draw()
    }

    pub async fn bootstrap(&mut self) -> io::Result<()> {
        self.client
            .initialize("cool-tui", env!("CARGO_PKG_VERSION"))
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        match self.client.list_sessions(None, 30).await {
            Ok(result) => {
                let existing = result
                    .sessions
                    .first()
                    .map(|session| session.session_id.clone());
                self.state.sessions = result.sessions;
                match existing {
                    Some(session_id) => self.load_session(&session_id).await,
                    None => self.create_session(None).await,
                }
            }
            Err(error) => {
                self.state.notice(format!("session list failed: {error}"));
            }
        }
        Ok(())
    }

    async fn execute(&mut self, commands: Vec<TuiCommand>) {
        for command in commands {
            match command {
                TuiCommand::Prompt {
                    session_id,
                    text,
                    model,
                } => {
                    let key = self.idempotency_key("prompt");
                    match self
                        .client
                        .prompt(&key, &session_id, &text, model.as_deref())
                        .await
                    {
                        Ok(accepted) => {
                            self.state.run_id = Some(accepted.run_id);
                            self.state.canonical_run_id = None;
                            self.state.canonical = Default::default();
                            self.state.begin_prompt(text);
                        }
                        Err(error) => {
                            self.state.busy = false;
                            self.state.notice(format!("prompt failed: {error}"));
                        }
                    }
                }
                TuiCommand::Steer { run_id, text } => {
                    let key = self.idempotency_key("steer");
                    if let Err(error) = self.client.steer(&key, &run_id, &text).await {
                        self.state.notice(format!("steer failed: {error}"));
                    }
                }
                TuiCommand::Cancel { run_id } => {
                    let key = self.idempotency_key("cancel");
                    if let Err(error) = self.client.cancel_run(&key, &run_id, Some("user")).await {
                        self.state.notice(format!("cancel failed: {error}"));
                    }
                }
                TuiCommand::ResolveApproval {
                    approval_id,
                    revision,
                    decision,
                } => {
                    let key = self.idempotency_key("approval");
                    if let Err(error) = self
                        .client
                        .resolve_approval(&key, &approval_id, revision, decision)
                        .await
                    {
                        self.state.notice(format!("approval failed: {error}"));
                    }
                }
                TuiCommand::LoadSession { session_id } => {
                    self.load_session(&session_id).await;
                }
                TuiCommand::ForkSession { session_id, title } => {
                    let key = self.idempotency_key("fork");
                    match self
                        .client
                        .fork_session(&key, &session_id, title.as_deref())
                        .await
                    {
                        Ok(forked) => {
                            self.state.set_connected_session(forked.session_id.clone());
                            self.state.notice(format!(
                                "forked {} into {}",
                                forked.forked_from, forked.session_id
                            ));
                        }
                        Err(error) => self.state.notice(format!("fork failed: {error}")),
                    }
                }
                TuiCommand::CreateSession { title } => {
                    self.create_session(title).await;
                }
                TuiCommand::ListSessions => match self.client.list_sessions(None, 30).await {
                    Ok(result) => {
                        self.state.sessions = result.sessions;
                        self.state.session_cursor = 0;
                    }
                    Err(error) => self.state.notice(format!("session list failed: {error}")),
                },
                TuiCommand::RefreshStatus => match self.client.status().await {
                    Ok(status) => self.state.status = status,
                    Err(error) => self.state.notice(format!("status failed: {error}")),
                },
            }
        }
    }

    async fn create_session(&mut self, title: Option<String>) {
        let key = self.idempotency_key("session");
        match self
            .client
            .create_session(&key, title.as_deref(), None)
            .await
        {
            Ok(session_id) => {
                self.state.set_connected_session(session_id);
                self.state.mode = TuiMode::Chat;
            }
            Err(error) => self.state.notice(format!("session create failed: {error}")),
        }
    }

    async fn load_session(&mut self, session_id: &str) {
        if let Err(error) = self.client.load_session(session_id).await {
            self.state.notice(format!("session load failed: {error}"));
            return;
        }
        match self.client.session_history(session_id, 200).await {
            Ok(history) => {
                self.state.set_connected_session(session_id.to_owned());
                self.state.apply_history(history.items);
                self.state.notice(format!("resumed {session_id}"));
            }
            Err(error) => self
                .state
                .notice(format!("session history failed: {error}")),
        }
    }
}
