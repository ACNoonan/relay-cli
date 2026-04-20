//! Single-pane chat TUI that drives the `Worker`.
//!
//! Layout:
//! ┌ agent ring ────────────────────────────────────────┐
//! │ [Claude]   GPT   Codex                   session…  │
//! ├────────────────────────────────────────────────────┤
//! │ conversation log (scrollable)                      │
//! │ …                                                  │
//! ├────────────────────────────────────────────────────┤
//! │ > input                                            │
//! │ status line                                        │
//! └────────────────────────────────────────────────────┘
//!
//! Keybindings:
//!   Enter            send typed input to the active agent
//!   Shift+Right      rotate to next agent, auto-handoff last assistant turn
//!   Shift+Left       rotate to previous agent, auto-handoff last assistant turn
//!   Tab              rotate to next agent WITHOUT handoff (focus-only)
//!   Ctrl+N           clear conversation + all provider session state
//!   Ctrl+H           toggle auto-handoff-on-rotate
//!   Ctrl+L           dump the current rendered TUI buffer to a snapshot file
//!   PgUp/PgDn/Home/End   scroll conversation log
//!   Esc              clear input
//!   Ctrl+C / q(empty input)   quit

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{io, time::Duration};
use tokio::sync::mpsc;

use super::conversation::{Agent, Conversation, Role, TurnStatus};
use super::worker::{Worker, WorkerCommand, WorkerConfig, WorkerEvent, WorkerStatus};

pub async fn run(cfg: WorkerConfig, initial_prompt: Option<String>) -> Result<()> {
    let (ev_tx, mut ev_rx) = mpsc::channel::<WorkerEvent>(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>(64);

    let worker = Worker::new(cfg, ev_tx)?;
    let log_path = worker.log_path();
    let worker_task = tokio::spawn(async move {
        if let Err(err) = worker.run(cmd_rx).await {
            tracing::error!(%err, "chat worker exited with error");
        }
    });

    if let Some(prompt) = initial_prompt {
        let trimmed = prompt.trim();
        if !trimmed.is_empty() {
            cmd_tx
                .send(WorkerCommand::SendToActive {
                    prompt: trimmed.to_string(),
                })
                .await
                .ok();
        }
    }

    let result = run_ui(&mut ev_rx, &cmd_tx, log_path);
    // Ensure worker shuts down cleanly.
    let _ = cmd_tx.send(WorkerCommand::Quit).await;
    let _ = worker_task.await;
    result
}

#[derive(Default)]
struct UiState {
    conversation: Option<Conversation>,
    status: WorkerStatus,
    status_message: String,
    input: String,
    scroll: u16,
    /// When true we stay pinned to the bottom as new deltas arrive.
    follow_tail: bool,
    last_error: Option<String>,
    quit_requested: bool,
    /// Set to true when the user hits Ctrl+L; consumed by the main loop after the next draw.
    snapshot_requested: bool,
    /// Path of the last snapshot written — shown in the status bar so the user can find it.
    last_snapshot_path: Option<String>,
    /// Path of the worker's chat log, shown once at startup so the user knows where to look.
    log_path_banner: Option<String>,
}

fn run_ui(
    rx: &mut mpsc::Receiver<WorkerEvent>,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
    log_path: Option<camino::Utf8PathBuf>,
) -> Result<()> {
    enable_raw_mode().context("enabling raw mode for chat TUI")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating chat terminal")?;

    let mut state = UiState {
        status_message: match &log_path {
            Some(p) => format!("Ready. Type to chat — Shift+←/→ rotate, Ctrl+L snapshot. log: {p}"),
            None => "Ready. Type to chat — Shift+←/→ rotate, Ctrl+L snapshot.".to_string(),
        },
        follow_tail: true,
        log_path_banner: log_path.as_ref().map(|p| p.to_string()),
        ..UiState::default()
    };

    let result = (|| -> Result<()> {
        loop {
            drain_worker_events(rx, &mut state);
            if state.quit_requested {
                break;
            }

            terminal
                .draw(|f| render(f, &state))
                .context("drawing chat UI")?;

            // Honour a queued snapshot request AFTER the frame has been drawn, so the
            // snapshot captures exactly what the user was looking at.
            if state.snapshot_requested {
                state.snapshot_requested = false;
                match dump_buffer_snapshot(&mut terminal, &state) {
                    Ok(path) => {
                        state.status_message = format!("snapshot saved to {path}");
                        state.last_snapshot_path = Some(path);
                    }
                    Err(err) => {
                        state.status_message = format!("snapshot failed: {err}");
                    }
                }
            }

            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read() {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    if !handle_key(key.code, key.modifiers, &mut state, cmd_tx) {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    result
}

fn dump_buffer_snapshot(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &UiState,
) -> Result<String> {
    let buf = terminal.current_buffer_mut().clone();
    let area = buf.area();
    let mut out = String::new();
    out.push_str(&format!(
        "# relay chat TUI snapshot  {}x{}  ts:{}\n",
        area.width,
        area.height,
        chrono::Utc::now().to_rfc3339()
    ));
    if let Some(p) = &state.log_path_banner {
        out.push_str(&format!("# chat log: {p}\n"));
    }
    out.push_str("# ────────────────────────────────────────────────────────────\n");
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(area.x + x, area.y + y)];
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }

    // Choose snapshot destination: prefer the harness logs dir if we have a log_path_banner,
    // otherwise fall back to the temp dir so the snapshot always lands somewhere readable.
    let dest = match &state.log_path_banner {
        Some(p) => {
            let parent = std::path::Path::new(p)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::env::temp_dir());
            parent.join(format!(
                "relay-chat-snapshot-{}.txt",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            ))
        }
        None => std::env::temp_dir().join(format!(
            "relay-chat-snapshot-{}.txt",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        )),
    };
    std::fs::write(&dest, out).with_context(|| format!("writing snapshot to {dest:?}"))?;
    Ok(dest.to_string_lossy().into_owned())
}

fn drain_worker_events(rx: &mut mpsc::Receiver<WorkerEvent>, state: &mut UiState) {
    while let Ok(event) = rx.try_recv() {
        match event {
            WorkerEvent::ConversationUpdated(c) => {
                state.conversation = Some(c);
            }
            WorkerEvent::StatusChanged(s) => {
                state.status = s;
            }
            WorkerEvent::StatusMessage(m) => {
                state.status_message = m;
            }
            WorkerEvent::Error(e) => {
                state.last_error = Some(e);
            }
        }
    }
}

/// Returns false if the UI should exit.
fn handle_key(
    code: KeyCode,
    mods: KeyModifiers,
    state: &mut UiState,
    cmd_tx: &mpsc::Sender<WorkerCommand>,
) -> bool {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let shift = mods.contains(KeyModifiers::SHIFT);

    match (code, ctrl, shift) {
        (KeyCode::Char('c'), true, _) => {
            state.quit_requested = true;
            return false;
        }
        (KeyCode::Char('q'), false, false) if state.input.is_empty() => {
            state.quit_requested = true;
            return false;
        }
        (KeyCode::Char('n'), true, _) => {
            let _ = cmd_tx.try_send(WorkerCommand::NewConversation);
        }
        (KeyCode::Char('h'), true, _) => {
            let _ = cmd_tx.try_send(WorkerCommand::ToggleAutoHandoff);
        }
        (KeyCode::Char('l'), true, _) => {
            state.snapshot_requested = true;
        }
        (KeyCode::Right, _, true) => {
            if let Some(c) = &state.conversation {
                let to = c.active_agent.next();
                let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                    agent: to,
                    handoff_last_assistant: c.auto_handoff_enabled,
                });
            }
        }
        (KeyCode::Left, _, true) => {
            if let Some(c) = &state.conversation {
                let to = c.active_agent.prev();
                let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                    agent: to,
                    handoff_last_assistant: c.auto_handoff_enabled,
                });
            }
        }
        (KeyCode::Tab, _, _) => {
            if let Some(c) = &state.conversation {
                let to = c.active_agent.next();
                let _ = cmd_tx.try_send(WorkerCommand::RotateTo {
                    agent: to,
                    handoff_last_assistant: false,
                });
            }
        }
        (KeyCode::Enter, _, _) => {
            let text = std::mem::take(&mut state.input);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                state.follow_tail = true;
                let _ = cmd_tx.try_send(WorkerCommand::SendToActive {
                    prompt: trimmed.to_string(),
                });
            }
        }
        (KeyCode::Esc, _, _) => {
            state.input.clear();
        }
        (KeyCode::Backspace, _, _) => {
            state.input.pop();
        }
        (KeyCode::PageUp, _, _) => {
            state.follow_tail = false;
            state.scroll = state.scroll.saturating_sub(10);
        }
        (KeyCode::PageDown, _, _) => {
            state.scroll = state.scroll.saturating_add(10);
        }
        (KeyCode::Home, _, _) => {
            state.follow_tail = false;
            state.scroll = 0;
        }
        (KeyCode::End, _, _) => {
            state.follow_tail = true;
        }
        (KeyCode::Char(ch), _, _) => {
            state.input.push(ch);
        }
        _ => {}
    }
    true
}

fn render(f: &mut Frame<'_>, state: &UiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // agent ring chrome
            Constraint::Min(3),    // conversation log
            Constraint::Length(1), // visual breathing room — keeps the last message from
            //                        butting against the input box's top border
            Constraint::Length(3), // input box
            Constraint::Length(1), // status line
        ])
        .split(f.area());

    render_agent_ring(f, chunks[0], state);
    render_conversation(f, chunks[1], state);
    // chunks[2] deliberately left blank as a spacer row.
    render_input(f, chunks[3], state);
    render_status(f, chunks[4], state);
}

fn render_agent_ring(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let active = state
        .conversation
        .as_ref()
        .map(|c| c.active_agent)
        .unwrap_or_default();
    let auto_handoff = state
        .conversation
        .as_ref()
        .map(|c| c.auto_handoff_enabled)
        .unwrap_or(true);

    let mut spans: Vec<Span<'_>> = Vec::new();
    for (i, agent) in Agent::RING.iter().enumerate() {
        let active_here = *agent == active;
        let color = agent_color(*agent);
        let style = if active_here {
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color).add_modifier(Modifier::DIM)
        };
        let label = if active_here {
            format!(" ▶ {} ", agent.label())
        } else {
            format!("   {}   ", agent.label())
        };
        spans.push(Span::styled(label, style));
        if i < Agent::RING.len() - 1 {
            spans.push(Span::raw("  "));
        }
    }

    let session_hint = state
        .conversation
        .as_ref()
        .and_then(|c| c.sessions.session_id_for(active))
        .map(|id| format!("  session:{}", &id[..id.len().min(8)]))
        .unwrap_or_default();
    spans.push(Span::styled(
        format!(
            "   auto-handoff:{}",
            if auto_handoff { "on" } else { "off" }
        ),
        Style::default().add_modifier(Modifier::DIM),
    ));
    if !session_hint.is_empty() {
        spans.push(Span::styled(
            session_hint,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }

    let block = Block::default().borders(Borders::ALL).title(" relay chat ");
    let para = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(para, area);
}

fn render_conversation(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    if let Some(c) = &state.conversation {
        if c.turns.is_empty() {
            lines.push(Line::styled(
                "No turns yet. Type a prompt and press Enter.",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        for turn in &c.turns {
            let prefix = match turn.role {
                Role::User => "you".to_string(),
                Role::Handoff => format!("↪ handoff → {}", turn.agent.label().to_lowercase()),
                Role::Assistant => turn.agent.label().to_lowercase(),
                Role::System => "system".to_string(),
            };
            let prefix_style = match turn.role {
                Role::User => Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                Role::Handoff => Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
                Role::Assistant => Style::default()
                    .fg(agent_color(turn.agent))
                    .add_modifier(Modifier::BOLD),
                Role::System => Style::default().fg(Color::DarkGray),
            };
            let suffix = match turn.status {
                TurnStatus::Streaming => " …",
                TurnStatus::Error => " ⚠",
                TurnStatus::Complete => "",
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{prefix}{suffix}"), prefix_style),
                Span::raw(""),
            ]));
            for line in turn.content.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(line.to_string()),
                ]));
            }
            if turn.content.is_empty() && turn.status == TurnStatus::Streaming {
                lines.push(Line::from(vec![Span::styled(
                    "  (waiting for first token…)",
                    Style::default().add_modifier(Modifier::DIM),
                )]));
            }
            lines.push(Line::raw(""));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" conversation ");
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    // Paragraph.scroll counts *wrapped* rows, not logical lines. Computing scroll
    // against `lines.len()` undercounts when content wraps, which pushes the tail
    // off-screen. Estimate wrapped rows by ceil(line_width / inner_width).
    let wrapped_total: usize = lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            if w == 0 {
                1
            } else {
                w.div_ceil(inner_width)
            }
        })
        .sum();
    let scroll = if state.follow_tail {
        wrapped_total.saturating_sub(inner_height) as u16
    } else {
        state.scroll
    };
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

fn render_input(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let active = state
        .conversation
        .as_ref()
        .map(|c| c.active_agent)
        .unwrap_or_default();
    let title = format!(" → {} ", active.label());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(agent_color(active)))
        .title(title);
    let content = if state.input.is_empty() {
        Span::styled(
            "type a message, Enter to send",
            Style::default().add_modifier(Modifier::DIM),
        )
    } else {
        Span::raw(state.input.as_str())
    };
    let para = Paragraph::new(Line::from(vec![Span::raw("› "), content])).block(block);
    f.render_widget(para, area);
}

fn render_status(f: &mut Frame<'_>, area: Rect, state: &UiState) {
    let status_text = match &state.status {
        WorkerStatus::Idle => "idle".to_string(),
        WorkerStatus::Submitting { agent } => format!("submitting to {}…", agent.label()),
        WorkerStatus::Streaming { agent } => format!("streaming {}…", agent.label()),
        WorkerStatus::QueuedHandoff { to } => format!("handoff → {} queued", to.label()),
        WorkerStatus::Error { message } => format!("error: {}", truncate(message, 80)),
    };

    let err_suffix = state
        .last_error
        .as_ref()
        .map(|e| format!("  |  last error: {}", truncate(e, 60)))
        .unwrap_or_default();

    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", status_text),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw(" "),
        Span::styled(
            state.status_message.clone(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::styled(err_suffix, Style::default().fg(Color::Red)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn agent_color(agent: Agent) -> Color {
    match agent {
        Agent::Claude => Color::Rgb(255, 140, 66),
        Agent::Gpt => Color::Rgb(16, 163, 127),
        Agent::Codex => Color::Rgb(120, 132, 255),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
