//! Ratatui rendering for the TUI state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::state::{TranscriptEntry, TuiMode, TuiState};

pub fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(if state.approval.is_some() { 4 } else { 0 }),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(area);

    render_header(frame, chunks[0], state);
    render_body(frame, chunks[1], state);
    if state.approval.is_some() {
        render_approval(frame, chunks[2], state);
    }
    render_notice(frame, chunks[3], state);
    render_input(frame, chunks[4], state);

    match state.mode {
        TuiMode::Sessions => render_sessions(frame, centered(area, 60, 60), state),
        TuiMode::Help => render_help(frame, centered(area, 60, 60)),
        TuiMode::Chat => {}
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let session = state.session_id.as_deref().unwrap_or("no session");
    let run = match (&state.run_id, state.busy) {
        (Some(run), true) => format!("{} running", shorten(run, 18)),
        (Some(run), false) => format!("{} idle", shorten(run, 18)),
        (None, _) => "idle".to_owned(),
    };
    let model = state.model.as_deref().unwrap_or("server default");
    let plugins = state.canonical.plugins.len();
    let workers = state.canonical.workers.len();
    let line = Line::from(vec![
        Span::styled(
            " cool ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {session}")),
        Span::styled(format!("  [{run}]"), Style::default().fg(Color::Yellow)),
        Span::raw(format!("  model={model}")),
        Span::styled(
            format!("  events={plugins}/{workers}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_body(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut text = Text::default();
    for entry in &state.transcript {
        match entry {
            TranscriptEntry::User(content) => {
                text.push_line(Line::styled(
                    format!("you: {content}"),
                    Style::default().fg(Color::Cyan),
                ));
            }
            TranscriptEntry::Assistant(content) => {
                text.push_line(Line::raw(format!("cool: {content}")));
            }
            TranscriptEntry::Reasoning(content) => {
                text.push_line(Line::styled(
                    format!("thinking: {}", shorten(content, 200)),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            TranscriptEntry::Tool {
                name,
                status,
                summary,
                ..
            } => {
                text.push_line(Line::styled(
                    format!(
                        "tool {name} [{status}]{}",
                        summary
                            .as_deref()
                            .map_or(String::new(), |value| format!(" {value}"))
                    ),
                    Style::default().fg(match status.as_str() {
                        "completed" => Color::Green,
                        "failed" => Color::Red,
                        _ => Color::Yellow,
                    }),
                ));
            }
            TranscriptEntry::Notice(message) => {
                text.push_line(Line::styled(
                    format!("· {message}"),
                    Style::default().fg(Color::Magenta),
                ));
            }
        }
    }
    if state.busy && !state.canonical.content.is_empty() {
        text.push_line(Line::raw(format!("cool: {}", state.canonical.content)));
    }
    if state.busy && !state.canonical.reasoning.is_empty() {
        text.push_line(Line::styled(
            format!("thinking: {}", state.canonical.reasoning),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if state.busy {
        let mut live = state
            .canonical
            .tools
            .iter()
            .map(|(call_id, status)| {
                let name = state
                    .tool_names
                    .get(call_id)
                    .map(String::as_str)
                    .unwrap_or(call_id);
                format!("{name}={status}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        if live.is_empty() {
            live = "waiting for model".to_owned();
        }
        text.push_line(Line::styled(
            format!("… {live}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(plan_id) = &state.canonical.active_plan_id {
        text.push_line(Line::styled(
            format!(
                "plan {plan_id}: {}/{} {}",
                state.canonical.plan_completed_steps,
                state.canonical.plan_total_steps,
                state.canonical.plan_status.as_deref().unwrap_or("planned")
            ),
            Style::default().fg(Color::Blue),
        ));
    }
    if text.lines.is_empty() {
        text.push_line(Line::styled(
            "type a prompt, Tab to browse sessions, /help for commands",
            Style::default().fg(Color::DarkGray),
        ));
    }
    let scroll = if state.auto_scroll {
        (text.lines.len() as u16).saturating_sub(area.height.saturating_sub(2))
    } else {
        0
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("conversation"))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

fn render_approval(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let Some(approval) = &state.approval else {
        return;
    };
    let lines = vec![
        Line::styled(
            format!("tool {} requests approval", approval.name),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(format!("reason: {}", approval.reason)),
        Line::raw(shorten(&approval.arguments, 160)),
        Line::styled(
            "[a] allow once    [d] deny",
            Style::default().fg(Color::Green),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("approval"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_notice(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let line = match &state.notice {
        Some(notice) => Line::styled(notice.clone(), Style::default().fg(Color::Magenta)),
        None => Line::styled(state.status_summary(), Style::default().fg(Color::DarkGray)),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_input(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let hint = if state.busy {
        "enter=steer  esc=cancel"
    } else {
        "enter=send  tab=sessions  /help"
    };
    let input = Paragraph::new(state.input.clone())
        .block(Block::default().borders(Borders::ALL).title(hint))
        .wrap(Wrap { trim: false });
    frame.render_widget(input, area);
    let cursor_x = area.x + 1 + (state.cursor as u16).min(area.width.saturating_sub(2));
    let cursor_y = area.y + 1;
    if area.width > 2 && area.height > 2 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_sessions(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    frame.render_widget(Clear, area);
    let items = if state.sessions.is_empty() {
        vec![ListItem::new("no sessions yet; /new creates one")]
    } else {
        state
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let title = session.title.as_deref().unwrap_or("(untitled)");
                let marker = if Some(&session.session_id) == state.session_id.as_ref() {
                    "*"
                } else {
                    " "
                };
                let line = format!(
                    "{marker} {} {title} [{}]",
                    shorten(&session.session_id, 20),
                    session
                        .active_run_id
                        .as_deref()
                        .map_or("idle", |_| "active")
                );
                let item = ListItem::new(line);
                if index == state.session_cursor {
                    item.style(Style::default().bg(Color::DarkGray))
                } else {
                    item
                }
            })
            .collect()
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("sessions: ↑/↓ select  enter=resume  f=fork  esc=close"),
        ),
        area,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Clear, area);
    let text = vec![
        Line::styled(
            "Cool TUI commands",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("/new [title]      start a session"),
        Line::raw("/sessions         browse and resume sessions"),
        Line::raw("/fork [title]     branch the current session"),
        Line::raw("/status           plugin, worker and MCP status"),
        Line::raw("/model [name]     show or switch the model"),
        Line::raw("/cancel           cancel the active run"),
        Line::raw("/steer <text>     redirect the active run"),
        Line::raw("/retry            resend the last prompt"),
        Line::raw("/quit             leave the TUI"),
        Line::raw("esc / ctrl-c      cancel a run, or quit when idle"),
        Line::raw("tab               session picker"),
        Line::raw("a / d             approve or deny a tool request"),
    ];
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title("help: any key closes"),
        ),
        area,
    );
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn shorten(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let head = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    format!("{head}…")
}
