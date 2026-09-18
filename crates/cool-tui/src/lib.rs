//! Ratatui TUI client for the Cool App Protocol.
//!
//! The TUI is a protocol client: it does not read the SQLite store and contains
//! no agent-loop business logic. State transitions use the shared canonical
//! `ClientState` reducer contract.

mod app;
mod state;
pub mod ui;

pub use app::{TuiApp, TuiCommand, TuiEvent};
pub use state::{PendingApproval, TranscriptEntry, TuiKey, TuiMode, TuiState};

use std::io;

use cool_app_server::AppClient;
use cool_protocol::EventEnvelope;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{
    self as crossterm_event, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::{cursor, execute, terminal};
use tokio::sync::{broadcast, mpsc};

/// Runs the interactive terminal loop until the user quits.
pub async fn run_terminal(client: AppClient) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        EnableBracketedPaste,
        cursor::Hide
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    let result = run_terminal_loop(terminal, client).await;
    terminal::disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        terminal::LeaveAlternateScreen,
        cursor::Show
    )?;
    result
}

/// Terminal-generic interactive loop. Tests embed this with a `TestBackend`.
pub async fn run_terminal_loop<B: Backend + 'static>(
    terminal: Terminal<B>,
    client: AppClient,
) -> io::Result<()> {
    let size = terminal.size()?;
    let (sender, receiver) = mpsc::channel::<TuiEvent>(256);
    let input_sender = sender.clone();
    std::thread::spawn(move || {
        loop {
            let event = match crossterm_event::read() {
                Ok(Event::Key(key)) => TuiEvent::Key(convert_key(key)),
                Ok(Event::Paste(text)) => TuiEvent::Key(TuiKey::Paste(text)),
                Ok(Event::Resize(width, height)) => TuiEvent::Resize(width, height),
                Ok(_) => continue,
                Err(_) => break,
            };
            if input_sender.blocking_send(event).is_err() {
                break;
            }
        }
    });
    let events = client.subscribe();
    let mut app = TuiApp::new(terminal, client, size.width, size.height);
    app.bootstrap().await?;
    run_event_loop(app, receiver, events).await
}

/// Runs the app until the user quits, cancelling an active run on shutdown.
/// The caller must have completed [`TuiApp::bootstrap`].
pub async fn run_event_loop<B: Backend + 'static>(
    mut app: TuiApp<B>,
    mut receiver: mpsc::Receiver<TuiEvent>,
    mut events: broadcast::Receiver<EventEnvelope>,
) -> io::Result<()> {
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            input = receiver.recv() => {
                let Some(event) = input else { break };
                handle(&mut app, event).await?;
            }
            event = events.recv() => {
                match event {
                    Ok(envelope) => handle(&mut app, TuiEvent::Server(Box::new(envelope))).await?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        handle(&mut app, TuiEvent::Disconnected(format!(
                            "event stream lagged by {skipped} events"
                        ))).await?;
                        events = app.event_receiver();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        events = app.event_receiver();
                    }
                }
            }
            _ = ticker.tick() => {
                handle(&mut app, TuiEvent::Tick).await?;
            }
        }
        if app.state.should_quit {
            break;
        }
    }
    if app.state.busy
        && let Some(run_id) = app.state.run_id.clone()
    {
        let key = cool_app_server::client::new_idempotency_key("tui-exit");
        let _ = app.client.cancel_run(&key, &run_id, Some("shutdown")).await;
    }
    Ok(())
}

async fn handle<B: Backend>(app: &mut TuiApp<B>, event: TuiEvent) -> io::Result<()> {
    app.handle(event).await
}

pub fn convert_key(key: KeyEvent) -> TuiKey {
    if key.kind == KeyEventKind::Release {
        return TuiKey::None;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(character) if control => TuiKey::Ctrl(character.to_ascii_lowercase()),
        KeyCode::Char(character) => TuiKey::Char(character),
        KeyCode::Enter => TuiKey::Enter,
        KeyCode::Backspace => TuiKey::Backspace,
        KeyCode::Delete => TuiKey::Delete,
        KeyCode::Left => TuiKey::Left,
        KeyCode::Right => TuiKey::Right,
        KeyCode::Up => TuiKey::Up,
        KeyCode::Down => TuiKey::Down,
        KeyCode::PageUp => TuiKey::PageUp,
        KeyCode::PageDown => TuiKey::PageDown,
        KeyCode::Home => TuiKey::Home,
        KeyCode::End => TuiKey::End,
        KeyCode::Tab => TuiKey::Tab,
        KeyCode::Esc => TuiKey::Esc,
        _ => TuiKey::None,
    }
}
