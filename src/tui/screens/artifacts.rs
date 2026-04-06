use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::state::{AppState, Pane};
use crate::tui::theme::Styles;
use crate::tui::widgets::chrome::{render_empty, retro_block};
use crate::tui::widgets::table::render_table;

pub fn render(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    if state.data.artifacts.is_empty() {
        render_empty(
            f,
            area,
            "ARTIFACTS",
            "NO ARTIFACTS AVAILABLE",
            styles,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    render_artifact_list(f, chunks[0], state, styles);
    render_artifact_preview(f, chunks[1], state, styles);
}

fn render_artifact_list(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let headers = &["ID", "TYPE", "CREATED", "PATH"];
    let widths = &[
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Length(18),
        Constraint::Min(20),
    ];

    let rows: Vec<Vec<String>> = state
        .data
        .artifacts
        .iter()
        .map(|a| {
            vec![
                a.short_id.clone(),
                a.artifact_type.clone(),
                a.created_at.format("%Y-%m-%d %H:%M").to_string(),
                truncate_path(&a.path, 30),
            ]
        })
        .collect();

    render_table(
        f,
        area,
        "ARTIFACTS",
        headers,
        widths,
        rows,
        state.artifact_index,
        state.focus == Pane::Left,
        styles,
    );
}

fn render_artifact_preview(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let artifact = match state.selected_artifact() {
        Some(a) => a,
        None => {
            render_empty(f, area, "PREVIEW", "SELECT AN ARTIFACT", styles);
            return;
        }
    };

    let block = retro_block("PREVIEW", state.focus == Pane::Right, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Show artifact metadata + first lines of content
    let mut lines = vec![
        Line::from(vec![
            Span::styled("  ID    ", styles.label()),
            Span::styled(artifact.id.to_string(), styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  TYPE  ", styles.label()),
            Span::styled(artifact.artifact_type.clone(), styles.accent()),
        ]),
        Line::from(vec![
            Span::styled("  PATH  ", styles.label()),
            Span::styled(artifact.path.clone(), styles.dim()),
        ]),
        Line::from(Span::styled(
            "  ──────────────────────────────────",
            styles.dim(),
        )),
    ];

    // Try to read first ~20 lines of the artifact
    let path = std::path::Path::new(&artifact.path);
    if path.is_file() {
        if let Ok(content) = std::fs::read_to_string(path) {
            let max_lines = (inner.height as usize).saturating_sub(5);
            for line in content.lines().take(max_lines) {
                let truncated: String = line.chars().take(inner.width as usize - 4).collect();
                lines.push(Line::from(Span::styled(
                    format!("  {}", truncated),
                    styles.base(),
                )));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  FILE NOT FOUND",
            styles.dim(),
        )));
    }

    let paragraph = Paragraph::new(lines).style(styles.base());
    f.render_widget(paragraph, inner);
}

fn truncate_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        path.to_string()
    } else {
        format!("...{}", &path[path.len() - max + 3..])
    }
}
