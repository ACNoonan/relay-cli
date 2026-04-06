use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::state::AppState;
use crate::tui::theme::Styles;
use crate::tui::widgets::chrome::{render_empty, retro_block};

pub fn render(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),  // system status
            Constraint::Length(7),  // session summary
            Constraint::Min(6),    // recent activity
        ])
        .split(area);

    render_system_status(f, chunks[0], state, styles);
    render_session_summary(f, chunks[1], state, styles);
    render_recent_activity(f, chunks[2], state, styles);
}

fn render_system_status(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let block = retro_block("SYSTEM STATUS", false, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let overview = &state.data.overview;

    let harness = if overview.harness_initialized {
        Span::styled("INITIALIZED", styles.success())
    } else {
        Span::styled("NOT INITIALIZED", styles.danger())
    };

    let git = if overview.git_repo {
        let branch = overview.git_branch.as_deref().unwrap_or("unknown");
        let dirty = if overview.git_dirty { " (dirty)" } else { "" };
        Span::styled(
            format!("{}{}", branch, dirty),
            if overview.git_dirty {
                styles.warning()
            } else {
                styles.success()
            },
        )
    } else {
        Span::styled("NO GIT REPO", styles.dim())
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  HARNESS       ", styles.label()),
            harness,
        ]),
        Line::from(vec![
            Span::styled("  GIT           ", styles.label()),
            git,
        ]),
        Line::from(Span::styled(
            "  ────────────────────────────────────────────",
            styles.dim(),
        )),
        Line::from(Span::styled("  PROVIDERS", styles.label())),
    ];

    for check in &overview.provider_checks {
        let install_icon = if check.installed { "+" } else { "-" };
        let auth_icon = if check.auth { "+" } else { "-" };
        let install_style = if check.installed {
            styles.success()
        } else {
            styles.danger()
        };
        let auth_style = if check.auth {
            styles.success()
        } else {
            styles.dim()
        };

        lines.push(Line::from(vec![
            Span::styled(format!("    {:<12}", check.name), styles.base()),
            Span::styled(format!("{} INSTALL  ", install_icon), install_style),
            Span::styled(format!("{} AUTH", auth_icon), auth_style),
        ]));
    }

    let paragraph = Paragraph::new(lines).style(styles.base());
    f.render_widget(paragraph, inner);
}

fn render_session_summary(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let block = retro_block("SESSION SUMMARY", false, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let counts = &state.data.overview.session_counts;

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  TOTAL     ", styles.label()),
            Span::styled(format!("{}", counts.total()), styles.accent_bold()),
            Span::styled("     ", styles.base()),
            Span::styled("RUNNING   ", styles.label()),
            Span::styled(
                format!("{}", counts.running),
                styles.status_style("Running"),
            ),
        ]),
        Line::from(vec![
            Span::styled("  COMPLETED ", styles.label()),
            Span::styled(
                format!("{}", counts.completed),
                styles.status_style("Completed"),
            ),
            Span::styled("     ", styles.base()),
            Span::styled("CRASHED   ", styles.label()),
            Span::styled(
                format!("{}", counts.crashed),
                styles.status_style("Crashed"),
            ),
        ]),
        Line::from(vec![
            Span::styled("  STOPPED   ", styles.label()),
            Span::styled(
                format!("{}", counts.stopped),
                styles.status_style("Stopped"),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(lines).style(styles.base());
    f.render_widget(paragraph, inner);
}

fn render_recent_activity(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_recent_sessions(f, chunks[0], state, styles);
    render_recent_artifacts_and_reviews(f, chunks[1], state, styles);
}

fn render_recent_sessions(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let overview = &state.data.overview;
    if overview.recent_sessions.is_empty() {
        render_empty(f, area, "RECENT SESSIONS", "NO SESSIONS RECORDED", styles);
        return;
    }

    let block = retro_block("RECENT SESSIONS", false, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![Line::from(vec![
        Span::styled("  ID        ", styles.label()),
        Span::styled("PROVIDER    ", styles.label()),
        Span::styled("STATUS  ", styles.label()),
    ])];

    for s in &overview.recent_sessions {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<10}", s.short_id), styles.base()),
            Span::styled(format!("{:<12}", s.provider), styles.base()),
            Span::styled(
                format!("{:<8}", s.status),
                styles.status_style(&s.status),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines).style(styles.base());
    f.render_widget(paragraph, inner);
}

fn render_recent_artifacts_and_reviews(
    f: &mut Frame,
    area: Rect,
    state: &AppState,
    styles: &Styles,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Recent artifacts
    let overview = &state.data.overview;
    if overview.recent_artifacts.is_empty() {
        render_empty(f, chunks[0], "RECENT ARTIFACTS", "NO ARTIFACTS AVAILABLE", styles);
    } else {
        let block = retro_block("RECENT ARTIFACTS", false, styles);
        let inner = block.inner(chunks[0]);
        f.render_widget(block, chunks[0]);

        let lines: Vec<Line> = overview
            .recent_artifacts
            .iter()
            .map(|a| {
                Line::from(vec![
                    Span::styled(format!("  {:<10}", a.short_id), styles.base()),
                    Span::styled(format!("{:<16}", a.artifact_type), styles.dim()),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines).style(styles.base());
        f.render_widget(paragraph, inner);
    }

    // Recent reviews
    if overview.recent_reviews.is_empty() {
        render_empty(f, chunks[1], "RECENT REVIEWS", "NO REVIEWS AVAILABLE", styles);
    } else {
        let block = retro_block("RECENT REVIEWS", false, styles);
        let inner = block.inner(chunks[1]);
        f.render_widget(block, chunks[1]);

        let lines: Vec<Line> = overview
            .recent_reviews
            .iter()
            .map(|r| {
                Line::from(vec![
                    Span::styled(format!("  {:<10}", r.short_id), styles.base()),
                    Span::styled(
                        format!("{:<12}", r.verdict),
                        styles.status_style(&r.verdict),
                    ),
                    Span::styled(
                        r.goal.chars().take(20).collect::<String>(),
                        styles.dim(),
                    ),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines).style(styles.base());
        f.render_widget(paragraph, inner);
    }
}
