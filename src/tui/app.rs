use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};

use super::actions::handle_action;
use super::data;
use super::events::poll_event;
use super::screens;
use super::state::{AppState, Screen};
use super::theme::Styles;
use super::widgets::chrome;

const POLL_TIMEOUT: Duration = Duration::from_millis(50);
const DATA_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
const LOG_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

pub fn run_app(harness_root: Utf8PathBuf) -> Result<()> {
    // Terminal setup
    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("entering alternate screen")?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating terminal")?;

    // Install panic hook for terminal cleanup
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(info);
    }));

    let mut state = AppState::new();
    let styles = Styles::new();

    // Initial data load
    state.data = data::load_snapshot(&harness_root);
    state.last_refresh = Some(chrono::Utc::now());
    state.clamp_indices();

    // Load initial logs if sessions exist
    if let Some(session) = state.log_session() {
        state.data.log_buffer =
            data::load_logs(&harness_root, session.id, state.log_source);
    }

    let mut last_data_refresh = Instant::now();
    let mut last_log_refresh = Instant::now();

    // Main loop
    while state.running {
        // Render
        terminal.draw(|f| {
            let size = f.area();

            let main_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // header
                    Constraint::Length(1), // nav
                    Constraint::Min(10),   // content
                    Constraint::Length(1), // status bar
                ])
                .split(size);

            chrome::render_header(f, main_layout[0], &state, &styles);
            chrome::render_nav(f, main_layout[1], &state, &styles);

            match state.screen {
                Screen::Overview => {
                    screens::overview::render(f, main_layout[2], &state, &styles)
                }
                Screen::Sessions => {
                    screens::sessions::render(f, main_layout[2], &state, &styles)
                }
                Screen::Logs => {
                    screens::logs::render(f, main_layout[2], &state, &styles)
                }
                Screen::Artifacts => {
                    screens::artifacts::render(f, main_layout[2], &state, &styles)
                }
                Screen::Reviews => {
                    screens::reviews::render(f, main_layout[2], &state, &styles)
                }
            }

            chrome::render_status_bar(f, main_layout[3], &state, &styles);

            if state.show_help {
                chrome::render_help(f, size, &styles);
            }
        })?;

        // Event handling
        let action = poll_event(POLL_TIMEOUT);
        let needs_refresh = handle_action(&mut state, action);

        // Data refresh
        let now = Instant::now();
        let should_refresh_data = needs_refresh
            || now.duration_since(last_data_refresh) >= DATA_REFRESH_INTERVAL;
        let should_refresh_logs = state.screen == Screen::Logs
            && now.duration_since(last_log_refresh) >= LOG_REFRESH_INTERVAL;

        if should_refresh_data {
            state.data = data::load_snapshot(&harness_root);
            state.last_refresh = Some(chrono::Utc::now());
            state.clamp_indices();
            last_data_refresh = now;

            if state.status_message.as_deref() == Some("REFRESHING...") {
                state.status_message = None;
            }
        }

        if should_refresh_logs || (needs_refresh && state.screen == Screen::Logs) {
            if let Some(session) = state.log_session() {
                let sid = session.id;
                state.data.log_buffer =
                    data::load_logs(&harness_root, sid, state.log_source);
            }
            last_log_refresh = now;
        }
    }

    // Terminal cleanup
    disable_raw_mode().context("disabling raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("leaving alternate screen")?;
    terminal.show_cursor().context("showing cursor")?;

    Ok(())
}
