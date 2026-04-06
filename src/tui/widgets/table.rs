use ratatui::{
    layout::{Constraint, Rect},
    text::{Line, Span},
    widgets::{Row, Table, TableState},
    Frame,
};

use crate::tui::theme::Styles;
use crate::tui::widgets::chrome::retro_block;

/// Render a styled table with selection support.
pub fn render_table(
    f: &mut Frame,
    area: Rect,
    title: &str,
    headers: &[&str],
    widths: &[Constraint],
    rows: Vec<Vec<String>>,
    selected: usize,
    active: bool,
    styles: &Styles,
) {
    let block = retro_block(title, active, styles);

    let header_cells: Vec<Span> = headers
        .iter()
        .map(|h| Span::styled(h.to_string(), styles.label()))
        .collect();
    let header = Row::new(header_cells).style(styles.label()).height(1);

    let table_rows: Vec<Row> = rows
        .iter()
        .enumerate()
        .map(|(i, cols)| {
            let cells: Vec<Span> = cols
                .iter()
                .map(|c| {
                    // Color status cells
                    let style = if c == "Running" || c == "Completed" || c == "Crashed"
                        || c == "Stopped" || c == "Pass" || c == "Fail"
                        || c == "NeedsWork" || c == "Inconclusive"
                    {
                        styles.status_style(c)
                    } else if i == selected && active {
                        styles.selected()
                    } else {
                        styles.base()
                    };
                    Span::styled(c.clone(), style)
                })
                .collect();

            let row_style = if i == selected && active {
                styles.selected()
            } else {
                styles.base()
            };
            Row::new(cells).style(row_style)
        })
        .collect();

    let table = Table::new(table_rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(styles.selected())
        .highlight_symbol(">> ");

    let mut table_state = TableState::default();
    table_state.select(Some(selected));
    f.render_stateful_widget(table, area, &mut table_state);
}

/// Render a simple key-value detail panel.
pub fn render_detail_panel(
    f: &mut Frame,
    area: Rect,
    title: &str,
    fields: Vec<(&str, String)>,
    active: bool,
    styles: &Styles,
) {
    let block = retro_block(title, active, styles);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = fields
        .iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(format!("  {:<14} ", label), styles.label()),
                Span::styled(value.clone(), styles.base()),
            ])
        })
        .collect();

    let paragraph = ratatui::widgets::Paragraph::new(lines).style(styles.base());
    f.render_widget(paragraph, inner);
}
