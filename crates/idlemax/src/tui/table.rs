//! Delivery: the dashboard tables. Each tab says which columns may shrink and which go first.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use super::layout::{ellipsize, fit_columns, text_width};
use crate::cli::rows;

/// How one tab's table behaves on a narrow terminal.
pub struct Spec {
    pub header: &'static [&'static str],
    /// Free-text columns that may shrink; all others keep their natural width.
    pub flex: &'static [usize],
    /// Columns removed, in this order, when a flex column would get too narrow.
    pub drop: &'static [usize],
    /// Narrowest a flex column may get before a column is dropped.
    pub min_width: usize,
    /// What the tab says when it has no rows, including what to do next.
    pub empty: &'static str,
}

pub const DEVICES: Spec = Spec {
    header: &rows::DEVICE_HEADER,
    flex: &[0, 2],
    drop: &[9, 7, 8, 3, 6, 5], // POWER, SWAP, DISK, IDLE, MEM FREE, LOAD
    min_width: 20, // the guard reason is the point of this tab, so it outranks the metrics
    empty: "No device has sent a heartbeat yet. Start `idlemax agent` on a device.",
};
/// `rows::JOB_HEADER` with the last column renamed: a waiting job shows why instead of a result.
const JOB_COLUMNS: [&str; 9] = [
    "ID",
    "NAME",
    "STATUS",
    "DEVICE",
    "COST",
    "PRIO",
    "TRIES",
    "AGE",
    "RESULT / WHY",
];

pub const JOBS: Spec = Spec {
    header: &JOB_COLUMNS,
    flex: &[1, 3, 8],
    drop: &[5, 4, 6, 3], // PRIO, COST, TRIES, DEVICE
    min_width: 16,
    empty: "No jobs yet. Queue one with `idlemax job add <spec.toml>`.",
};
pub const QUOTA: Spec = Spec {
    header: &rows::QUOTA_HEADER,
    flex: &[0, 7],
    drop: &[3, 4], // RESERVE, HEADROOM
    min_width: 18,
    empty: "No AI accounts configured. Add [[quota.accounts]] to config.toml.",
};
pub const ADVICE: Spec = Spec {
    header: &rows::ADVICE_HEADER,
    flex: &[2],
    drop: &[4, 3], // SOURCE, CONF
    min_width: 24,
    empty: "Nothing needs you right now.",
};
pub const EVENTS: Spec = Spec {
    header: &rows::EVENT_HEADER,
    flex: &[2, 4],
    drop: &[2], // DEVICE
    min_width: 24,
    empty: "No events yet.",
};
pub const SCHEDULES: Spec = Spec {
    header: &rows::SCHEDULE_HEADER,
    flex: &[1, 3, 4],
    drop: &[],
    min_width: 14,
    empty: "No schedules yet. Repeat a job with `idlemax schedule add <spec.toml> --every 1d`.",
};

/// Draws the rows, or the tab's empty-state message when there are none.
pub fn table(
    f: &mut Frame,
    area: Rect,
    spec: &Spec,
    rows: Vec<(Vec<String>, Style)>,
    state: &mut TableState,
) {
    if rows.is_empty() {
        return empty_state(f, area, spec.empty);
    }
    let cell_len = |cells: &[String], c: usize| cells.get(c).map_or(0, |s| text_width(s));
    let natural: Vec<usize> = spec
        .header
        .iter()
        .enumerate()
        .map(|(c, h)| {
            rows.iter()
                .map(|(cells, _)| cell_len(cells, c))
                .fold(text_width(h), usize::max)
        })
        .collect();
    // Two columns go to the borders, two to the "> " marker, one keeps text off the right border.
    let avail = usize::from(area.width).saturating_sub(5);
    let fit = fit_columns(&natural, spec.flex, spec.drop, spec.min_width, avail);
    let head = Row::new(fit.keep.iter().map(|c| Cell::from(spec.header[*c])))
        .style(Style::new().bold().underlined());
    let body = rows.into_iter().map(|(cells, style)| {
        let shown = fit
            .keep
            .iter()
            .zip(&fit.widths)
            .map(|(c, w)| ellipsize(cells.get(*c).map_or("", String::as_str), *w));
        Row::new(shown.collect::<Vec<_>>()).style(style)
    });
    let widths = fit.widths.iter().map(|w| Constraint::Length(clamp_u16(*w)));
    let t = Table::new(body, widths)
        .header(head)
        .block(Block::bordered())
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2)
        .highlight_symbol("> ");
    f.render_stateful_widget(t, area, state);
}

/// A bordered box with a hint instead of an empty table.
pub fn empty_state(f: &mut Frame, area: Rect, text: &str) {
    let p = Paragraph::new(text.to_string())
        .dark_gray()
        .wrap(Wrap { trim: true })
        .block(Block::bordered());
    f.render_widget(p, area);
}

fn clamp_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}
