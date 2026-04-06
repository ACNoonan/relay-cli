use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

use crate::tui::state::{AppState, Pane};
use crate::tui::theme::Styles;
use crate::tui::widgets::chrome::render_empty;
use crate::tui::widgets::log_view::render_log_view;
use crate::tui::widgets::table::render_table;

pub fn render(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    if state.data.sessions.is_empty() {
        render_empty(
            f,
            area,
            "LOGS",
            "NO SESSIONS AVAILABLE",
            styles,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(30)])
        .split(area);

    render_session_picker(f, chunks[0], state, styles);
    render_log_pane(f, chunks[1], state, styles);
}

fn render_session_picker(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let headers = &["ID", "PROVIDER", "STATUS"];
    let widths = &[
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(10),
    ];

    let rows: Vec<Vec<String>> = state
        .data
        .sessions
        .iter()
        .map(|s| {
            vec![
                s.short_id.clone(),
                s.provider.clone(),
                s.status.clone(),
            ]
        })
        .collect();

    render_table(
        f,
        area,
        "SESSION",
        headers,
        widths,
        rows,
        state.log_session_index,
        state.focus == Pane::Left,
        styles,
    );
}

fn render_log_pane(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let title = format!("LOG OUTPUT [{}]", state.log_source.label());
    render_log_view(
        f,
        area,
        &title,
        &state.data.log_buffer,
        state.log_scroll,
        state.focus == Pane::Right,
        styles,
    );
}
