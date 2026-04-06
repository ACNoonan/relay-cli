use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::state::LogBuffer;
use crate::tui::theme::Styles;
use crate::tui::widgets::chrome::retro_block;

/// Render a scrollable log view.
pub fn render_log_view(
    f: &mut Frame,
    area: Rect,
    title: &str,
    buffer: &LogBuffer,
    scroll: usize,
    active: bool,
    styles: &Styles,
) {
    let block = retro_block(title, active, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if buffer.lines.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "NO LOG DATA",
            styles.dim(),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        let y = inner.y + inner.height / 3;
        if y < inner.y + inner.height {
            let msg_area = Rect::new(inner.x, y, inner.width, 1);
            f.render_widget(empty, msg_area);
        }
        return;
    }

    let visible_height = inner.height as usize;
    let total = buffer.lines.len();

    // Auto-scroll to bottom if scroll is beyond range
    let scroll_pos = if scroll >= total.saturating_sub(visible_height) {
        total.saturating_sub(visible_height)
    } else {
        scroll
    };

    let end = (scroll_pos + visible_height).min(total);
    let visible_lines: Vec<Line> = buffer.lines[scroll_pos..end]
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), styles.base())))
        .collect();

    let log = Paragraph::new(visible_lines).style(styles.base());
    f.render_widget(log, inner);
}
