//! Delivery: pure width fitting for the dashboard (no ratatui types), so an 80x24 terminal stays readable.

/// `s` cut to `max` characters, with an ellipsis when something was removed.
pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    if max > 0 {
        out.push('\u{2026}'); // horizontal ellipsis
    }
    out
}

/// Rows `text` needs at `width`, between 1 and `max`.
pub fn wrapped_lines(text: &str, width: usize, max: usize) -> usize {
    text.chars()
        .count()
        .div_ceil(width.max(1))
        .clamp(1, max.max(1))
}

/// Tab labels as "1 Devices (3)" when they all fit in `width`, else "1 Devices".
/// The widget adds one space of padding on each side and a one character divider.
pub fn tab_titles(tabs: &[(&str, usize)], width: usize) -> Vec<String> {
    let build = |counts: bool| -> Vec<String> {
        let label = |(i, (title, n)): (usize, &(&str, usize))| match counts {
            true => format!("{} {title} ({n})", i + 1),
            false => format!("{} {title}", i + 1),
        };
        tabs.iter().enumerate().map(label).collect()
    };
    let full = build(true);
    let needed: usize = full.iter().map(|t| t.chars().count() + 2).sum::<usize>() + full.len() - 1;
    if needed <= width {
        full
    } else {
        build(false)
    }
}

/// Key hints joined by two spaces. While they overflow `width`, the one with the
/// highest priority number is dropped, so `q quit` (priority 0) goes last.
pub fn fit_hints(hints: &[(u8, String)], width: usize) -> String {
    let mut keep: Vec<&(u8, String)> = hints.iter().collect();
    let total = |keep: &[&(u8, String)]| -> usize {
        keep.iter().map(|h| h.1.chars().count()).sum::<usize>() + 2 * keep.len().saturating_sub(1)
    };
    while keep.len() > 1 && total(&keep) > width {
        if let Some(worst) = (0..keep.len()).max_by_key(|&i| keep[i].0) {
            keep.remove(worst);
        }
    }
    keep.iter()
        .map(|h| h.1.as_str())
        .collect::<Vec<_>>()
        .join("  ")
}
