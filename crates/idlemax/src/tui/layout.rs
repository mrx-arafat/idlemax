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

/// `text` broken into lines of at most `width` characters at word boundaries (a word
/// longer than a line is split). Past `max_lines` the rest is cut and the last line
/// ends with an ellipsis, so the cause at the end of a long error is the only loss.
pub fn wrap_words(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > width {
            lines.extend((!line.is_empty()).then(|| std::mem::take(&mut line)));
            lines.push(word.chars().take(width).collect());
            word = word.chars().skip(width).collect();
        }
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines.max(1) {
        lines.truncate(max_lines.max(1));
        if let Some(last) = lines.last_mut() {
            let head: String = last.chars().take(width.saturating_sub(1)).collect();
            *last = head + "\u{2026}";
        }
    }
    lines
}

/// `parts` joined by " | ", as one line when it fits `width`, else as several lines
/// that break only between parts.
pub fn join_parts(parts: &[String], width: usize) -> Vec<String> {
    const SEP: &str = " | ";
    let mut lines: Vec<String> = Vec::new();
    for part in parts {
        match lines.last_mut() {
            Some(line) if line.chars().count() + SEP.len() + part.chars().count() <= width => {
                line.push_str(SEP);
                line.push_str(part);
            }
            _ => lines.push(part.clone()),
        }
    }
    lines
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

/// Space between table columns.
const GAP: usize = 2;

/// Which columns a table shows and how wide each one is.
#[derive(Debug, PartialEq, Eq)]
pub struct Fit {
    /// Column indexes that survive, in order.
    pub keep: Vec<usize>,
    /// Width of each kept column.
    pub widths: Vec<usize>,
}

/// Fits columns with natural widths `natural` into `avail` characters.
///
/// Only `flex` columns (free text) shrink, evenly; every other column keeps its
/// natural width so numbers and states are never cut. When a flex column would end
/// up narrower than `min_width`, the next column of `drop_order` is removed instead.
pub fn fit_columns(
    natural: &[usize],
    flex: &[usize],
    drop_order: &[usize],
    min_width: usize,
    avail: usize,
) -> Fit {
    let mut keep: Vec<usize> = (0..natural.len()).collect();
    let mut drops = drop_order.iter();
    loop {
        let widths = shrink(&keep, natural, flex, avail);
        let squeezed = keep
            .iter()
            .zip(&widths)
            .any(|(c, w)| flex.contains(c) && *w < natural[*c] && *w < min_width);
        let Some(next) = squeezed.then(|| drops.next()).flatten() else {
            return Fit { keep, widths };
        };
        keep.retain(|c| c != next);
    }
}

/// Widths for `keep`: flex columns share what the fixed ones and the gaps leave.
fn shrink(keep: &[usize], natural: &[usize], flex: &[usize], avail: usize) -> Vec<usize> {
    let fixed: usize = keep
        .iter()
        .filter(|c| !flex.contains(c))
        .map(|c| natural[*c])
        .sum();
    let room = avail.saturating_sub(GAP * keep.len().saturating_sub(1) + fixed);
    let wants: Vec<usize> = keep
        .iter()
        .filter(|c| flex.contains(c))
        .map(|c| natural[*c])
        .collect();
    let mut level = wants.iter().copied().max().unwrap_or(0);
    while level > 1 && wants.iter().map(|w| (*w).min(level)).sum::<usize>() > room {
        level -= 1;
    }
    // Hand out what the even cut left over, one character at a time.
    let mut spare = room.saturating_sub(wants.iter().map(|w| (*w).min(level)).sum());
    keep.iter()
        .map(|c| {
            if !flex.contains(c) {
                return natural[*c];
            }
            let extra = usize::from(natural[*c] > level && spare > 0);
            spare -= extra;
            natural[*c].min(level) + extra
        })
        .collect()
}
