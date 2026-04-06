use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};

use crate::tui::state::{AppState, Pane};
use crate::tui::theme::Styles;
use crate::tui::widgets::chrome::render_empty;
use crate::tui::widgets::table::{render_detail_panel, render_table};

pub fn render(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    if state.data.sessions.is_empty() {
        render_empty(
            f,
            area,
            "SESSIONS",
            "NO SESSIONS RECORDED",
            styles,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    render_session_list(f, chunks[0], state, styles);
    render_session_detail(f, chunks[1], state, styles);
}

fn render_session_list(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let headers = &["ID", "PROVIDER", "ROLE", "STATUS", "STARTED"];
    let widths = &[
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Min(16),
    ];

    let rows: Vec<Vec<String>> = state
        .data
        .sessions
        .iter()
        .map(|s| {
            vec![
                s.short_id.clone(),
                s.provider.clone(),
                s.role.clone(),
                s.status.clone(),
                s.started_at.format("%Y-%m-%d %H:%M").to_string(),
            ]
        })
        .collect();

    render_table(
        f,
        area,
        "SESSIONS",
        headers,
        widths,
        rows,
        state.session_index,
        state.focus == Pane::Left,
        styles,
    );
}

fn render_session_detail(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let detail = match state.selected_session_detail() {
        Some(d) => d,
        None => {
            render_empty(f, area, "DETAIL", "SELECT A SESSION", styles);
            return;
        }
    };

    let fields = vec![
        ("ID", detail.id.to_string()),
        ("PROVIDER", detail.provider.clone()),
        ("ROLE", detail.role.clone()),
        ("STATUS", detail.status.clone()),
        (
            "MODEL",
            detail.model.as_deref().unwrap_or("-").to_string(),
        ),
        ("CWD", detail.cwd.clone()),
        (
            "STARTED",
            detail.started_at.format("%Y-%m-%d %H:%M:%S").to_string(),
        ),
        (
            "STOPPED",
            detail
                .stopped_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "-".to_string()),
        ),
        ("ARTIFACT DIR", detail.artifact_dir.clone()),
        ("LOG DIR", detail.log_dir.clone()),
    ];

    render_detail_panel(
        f,
        area,
        "SESSION DETAIL",
        fields,
        state.focus == Pane::Right,
        styles,
    );
}
