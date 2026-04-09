use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::tui::state::{AppState, Screen};
use crate::tui::theme::Styles;

/// Render the top header bar.
pub fn render_header(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let now = chrono::Local::now().format("%H:%M:%S").to_string();

    let branch_info = if let Some(ref branch) = state.data.overview.git_branch {
        let dirty = if state.data.overview.git_dirty {
            "*"
        } else {
            ""
        };
        format!(" {}{}", branch, dirty)
    } else {
        String::new()
    };

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();

    let init_indicator = if state.data.overview.harness_initialized {
        Span::styled(" ONLINE ", styles.success())
    } else {
        Span::styled(" OFFLINE ", styles.danger())
    };

    let left = vec![
        Span::styled(" RELAY HARNESS ", styles.header()),
        Span::styled(" ", styles.base()),
        init_indicator,
    ];

    let center_text = format!("  {}  ", cwd);
    let right_text = format!("{}  {} ", branch_info, now);

    // Calculate padding
    let left_len: usize = left.iter().map(|s| s.width()).sum();
    let right_len = right_text.len();
    let center_len = center_text.len();
    let total = left_len + center_len + right_len;
    let pad = if area.width as usize > total {
        area.width as usize - total
    } else {
        1
    };
    let left_pad = pad / 2;
    let right_pad = pad - left_pad;

    let mut spans = left;
    spans.push(Span::styled(" ".repeat(left_pad), styles.header()));
    spans.push(Span::styled(center_text, styles.accent()));
    spans.push(Span::styled(" ".repeat(right_pad), styles.header()));
    spans.push(Span::styled(right_text, styles.dim()));

    let header = Paragraph::new(Line::from(spans)).style(styles.header());
    f.render_widget(header, area);
}

/// Render the navigation strip below the header.
pub fn render_nav(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let mut spans = vec![Span::styled(" ", styles.base())];

    for screen in &Screen::ALL {
        let active = state.screen == *screen;
        let label = format!(" {} {} ", screen.key(), screen.label());

        if active {
            spans.push(Span::styled(label, styles.nav_item(true)));
        } else {
            spans.push(Span::styled(label, styles.nav_item(false)));
        }
        spans.push(Span::styled(" ", styles.base()));
    }

    let nav = Paragraph::new(Line::from(spans)).style(styles.base());
    f.render_widget(nav, area);
}

/// Render the bottom status bar.
pub fn render_status_bar(f: &mut Frame, area: Rect, state: &AppState, styles: &Styles) {
    let screen_label = state.screen.label();

    let keys = match state.screen {
        Screen::Overview => "q:QUIT  ?:HELP  r:REFRESH  1-5:SCREENS",
        Screen::Sessions => "q:QUIT  j/k:MOVE  ENTER:DETAIL  TAB:PANE  r:REFRESH",
        Screen::Logs => "q:QUIT  j/k:MOVE  t:TOGGLE SRC  TAB:PANE  r:REFRESH",
        Screen::Artifacts => "q:QUIT  j/k:MOVE  TAB:PANE  r:REFRESH",
        Screen::Reviews => "q:QUIT  j/k:MOVE  ENTER:DETAIL  TAB:PANE  r:REFRESH",
    };

    let refresh_text = state
        .last_refresh
        .map(|t| {
            let ago = chrono::Utc::now().signed_duration_since(t);
            if ago.num_seconds() < 2 {
                " JUST NOW".to_string()
            } else {
                format!(" {}s AGO", ago.num_seconds())
            }
        })
        .unwrap_or_default();

    let status = state.status_message.as_deref().unwrap_or("");

    let line = Line::from(vec![
        Span::styled(format!(" {} ", screen_label), styles.accent_bold()),
        Span::styled("  ", styles.status_bar()),
        Span::styled(keys, styles.dim()),
        Span::styled("  ", styles.status_bar()),
        Span::styled(status, styles.warning()),
        Span::styled(format!("{}  ", refresh_text), styles.dim()),
    ]);

    let bar = Paragraph::new(line).style(styles.status_bar());
    f.render_widget(bar, area);
}

/// Render a help overlay.
pub fn render_help(f: &mut Frame, area: Rect, styles: &Styles) {
    let help_text = vec![
        Line::from(Span::styled("KEYBINDINGS", styles.accent_bold())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  q        ", styles.accent()),
            Span::styled("Quit", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  ?        ", styles.accent()),
            Span::styled("Toggle help", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  1-5      ", styles.accent()),
            Span::styled("Switch screen", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  Tab      ", styles.accent()),
            Span::styled("Next pane", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  S-Tab    ", styles.accent()),
            Span::styled("Previous pane", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  j / k    ", styles.accent()),
            Span::styled("Move down / up", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  g / G    ", styles.accent()),
            Span::styled("Top / Bottom", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  Enter    ", styles.accent()),
            Span::styled("Select / Focus", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  Esc      ", styles.accent()),
            Span::styled("Back / Close", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  r        ", styles.accent()),
            Span::styled("Refresh data", styles.base()),
        ]),
        Line::from(vec![
            Span::styled("  t        ", styles.accent()),
            Span::styled("Toggle stdout/stderr (logs)", styles.base()),
        ]),
        Line::from(""),
        Line::from(Span::styled("  Press ? or Esc to close", styles.dim())),
    ];

    // Center the help box
    let width = 50u16.min(area.width.saturating_sub(4));
    let height = (help_text.len() as u16 + 2).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let help_area = Rect::new(x, y, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border_active())
        .style(styles.base())
        .title(Span::styled(" HELP ", styles.accent_bold()));

    let help = Paragraph::new(help_text).block(block).style(styles.base());
    f.render_widget(help, help_area);
}

/// Create a retro framed block.
pub fn retro_block<'a>(title: &'a str, active: bool, styles: &'a Styles) -> Block<'a> {
    let border_style = if active {
        styles.border_active()
    } else {
        styles.border()
    };

    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            format!(" {} ", title),
            if active {
                styles.accent_bold()
            } else {
                styles.label()
            },
        ))
        .style(styles.base())
}

/// Render an empty state message in a block.
pub fn render_empty(f: &mut Frame, area: Rect, title: &str, message: &str, styles: &Styles) {
    let block = retro_block(title, false, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let pad_top = inner.height / 3;
    if pad_top < inner.height {
        let msg_area = Rect::new(inner.x, inner.y + pad_top, inner.width, 1);
        let msg = Paragraph::new(Line::from(Span::styled(message, styles.dim())))
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(msg, msg_area);
    }
}
