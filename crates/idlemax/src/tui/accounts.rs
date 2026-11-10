//! Delivery: the "AI quota" panel under the devices table, one line per configured account:
//! whether idlemax may spend it, how much, and when its window resets.

use idlemax_core::capacity::AccountCapacity;
use idlemax_core::types::UnixMs;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Padding, Paragraph};
use ratatui::Frame;

use super::layout::wrap_words;
use crate::cli::rows::account_line;

/// Rows the devices table keeps; the panel gives way before it takes them.
const MIN_TABLE_ROWS: u16 = 6;

/// Longest an account's line may wrap to.
const MAX_LINES_EACH: usize = 2;

/// Draws the panel at the bottom and returns the area left for the table. Nothing is drawn,
/// and the whole area is returned, when no account is configured or the screen is too small.
pub fn render(f: &mut Frame, area: Rect, accounts: &[AccountCapacity], now_ms: UnixMs) -> Rect {
    let inner = usize::from(area.width.saturating_sub(4)).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for a in accounts {
        let style = match (a.paused, a.spendable) {
            (false, true) => Style::new().green(),
            _ => Style::new(),
        };
        let text = account_line(a, now_ms);
        lines.extend(
            wrap_words(&text, inner, MAX_LINES_EACH)
                .into_iter()
                .map(|l| Line::styled(l, style)),
        );
    }
    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
    if lines.is_empty() || area.height < height.saturating_add(MIN_TABLE_ROWS) {
        return area;
    }
    let [rest, panel] =
        Layout::vertical([Constraint::Min(MIN_TABLE_ROWS), Constraint::Length(height)]).areas(area);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(" AI quota ");
    f.render_widget(Paragraph::new(lines).block(block), panel);
    rest
}
