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
    if state.data.reviews.is_empty() {
        render_empty(f, area, "REVIEWS", "NO REVIEWS AVAILABLE", styles);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_review_list(f, chunks[0], state, styles);
    render_review_detail(f, chunks[1], state, styles);
}

fn render_review_list(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let headers = &["ID", "PROVIDER", "VERDICT", "FINDINGS", "DATE"];
    let widths = &[
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(9),
        Constraint::Min(12),
    ];

    let rows: Vec<Vec<String>> = state
        .data
        .reviews
        .iter()
        .map(|r| {
            vec![
                r.short_id.clone(),
                r.provider.clone(),
                r.verdict.clone(),
                r.finding_count.to_string(),
                r.created_at.format("%Y-%m-%d").to_string(),
            ]
        })
        .collect();

    render_table(
        f,
        area,
        "REVIEWS",
        headers,
        widths,
        rows,
        state.review_index,
        state.focus == Pane::Left,
        styles,
    );
}

fn render_review_detail(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let detail = match state.selected_review_detail() {
        Some(d) => d,
        None => {
            render_empty(f, area, "REVIEW DETAIL", "SELECT A REVIEW", styles);
            return;
        }
    };

    let block = retro_block("REVIEW DETAIL", state.focus == Pane::Right, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  VERDICT   ", styles.label()),
            Span::styled(detail.verdict.clone(), styles.status_style(&detail.verdict)),
        ]),
        Line::from(vec![
            Span::styled("  PROVIDER  ", styles.label()),
            Span::styled(detail.provider.clone(), styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  MODEL     ", styles.label()),
            Span::styled(
                detail.model.as_deref().unwrap_or("-").to_string(),
                styles.base(),
            ),
        ]),
        Line::from(vec![
            Span::styled("  DATE      ", styles.label()),
            Span::styled(
                detail.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                styles.base(),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled("  SUMMARY", styles.label())),
    ];

    // Word-wrap summary
    let max_width = inner.width.saturating_sub(4) as usize;
    for chunk in detail.summary.as_bytes().chunks(max_width) {
        let s = String::from_utf8_lossy(chunk);
        lines.push(Line::from(Span::styled(format!("  {}", s), styles.base())));
    }

    if !detail.findings.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  FINDINGS ({})", detail.findings.len()),
            styles.label(),
        )));
        lines.push(Line::from(Span::styled(
            "  ──────────────────────────────────",
            styles.dim(),
        )));

        let max_findings = (inner.height as usize).saturating_sub(lines.len() + 1);
        for (i, finding) in detail.findings.iter().take(max_findings).enumerate() {
            let loc = match (&finding.file, finding.line) {
                (Some(file), Some(line)) => format!(" {}:{}", file, line),
                (Some(file), None) => format!(" {}", file),
                _ => String::new(),
            };

            lines.push(Line::from(vec![
                Span::styled(format!("  {}. ", i + 1), styles.dim()),
                Span::styled(
                    format!("[{}]", finding.severity.to_uppercase()),
                    styles.severity_style(&finding.severity),
                ),
                Span::styled(format!(" {}", finding.category), styles.accent()),
                Span::styled(loc, styles.dim()),
            ]));

            let msg: String = finding.message.chars().take(max_width - 6).collect();
            lines.push(Line::from(Span::styled(
                format!("     {}", msg),
                styles.base(),
            )));

            if let Some(suggestion) = &finding.suggestion {
                let suggestion_line: String = suggestion.chars().take(max_width - 6).collect();
                lines.push(Line::from(Span::styled(
                    format!("     suggestion: {}", suggestion_line),
                    styles.dim(),
                )));
            }
        }
    }

    let paragraph = Paragraph::new(lines).style(styles.base());
    f.render_widget(paragraph, inner);
}
