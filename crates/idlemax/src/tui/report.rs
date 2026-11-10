//! Delivery: the "Did it help?" panel at the top of the Devices tab, the same verdict lines
//! `idlemax report` prints.

use idlemax_core::report::Report;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Padding, Paragraph};
use ratatui::Frame;

use super::layout::wrap_words;

/// Rows the devices table keeps; the panel gives way before it takes them.
const MIN_TABLE_ROWS: u16 = 6;

/// Draws one wrapped verdict per report above the table and returns the area left for the
/// table. A period in which nothing ran is yellow.
pub fn render(f: &mut Frame, area: Rect, reports: &[Report]) -> Rect {
    let inner = usize::from(area.width.saturating_sub(4)).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for r in reports {
        let style = if r.jobs.ran == 0 {
            Style::new().yellow()
        } else {
            Style::new()
        };
        let wrapped = wrap_words(&r.verdict, inner, 2);
        lines.extend(wrapped.into_iter().map(|l| Line::styled(l, style)));
    }
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    if lines.is_empty() || area.height < height.saturating_add(MIN_TABLE_ROWS) {
        return area;
    }
    let [panel, rest] =
        Layout::vertical([Constraint::Length(height), Constraint::Min(MIN_TABLE_ROWS)]).areas(area);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(" Did it help? ");
    f.render_widget(Paragraph::new(lines).block(block), panel);
    rest
}
