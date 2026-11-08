//! Delivery: pure width fitting for the dashboard (no ratatui types), so an 80x24 terminal stays readable.
//! Widths are display cells, not characters: Japanese text and emoji take two cells each.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ELLIPSIS: char = '\u{2026}'; // horizontal ellipsis

/// Terminal cells `s` takes.
pub fn text_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// The longest start of `s` that fits `max` cells; at least one character when `force`.
fn split_at_width(s: &str, max: usize, force: bool) -> (&str, &str) {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += c.width().unwrap_or(0);
        if used > max && (i > 0 || !force) {
            return s.split_at(i);
        }
    }
    (s, "")
}

/// `s` cut to `max` cells, with an ellipsis when something was removed.
pub fn ellipsize(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    let mut out = split_at_width(s, max.saturating_sub(1), false)
        .0
        .to_string();
    if max > 0 {
        out.push(ELLIPSIS);
    }
    out
}

/// One logical line broken at word boundaries into lines of at most `width` cells.
/// Continuation lines keep the line's leading indent, plus two cells under a "- " bullet;
/// a word wider than a whole line is split.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let body = line.trim_start();
    if body.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let indent = (line.len() - body.len()).min(width / 2);
    let hang = (indent + if body.starts_with("- ") { 2 } else { 0 }).min(width / 2);
    let mut lines: Vec<String> = Vec::new();
    let mut current = " ".repeat(indent);
    let mut prefix = indent; // cells of indent on the current line
    for word in body.split_whitespace() {
        let mut rest = word;
        while !rest.is_empty() {
            let used = text_width(&current);
            let has_words = used > prefix;
            let room = width.saturating_sub(used + usize::from(has_words));
            if text_width(rest) <= room {
                if has_words {
                    current.push(' ');
                }
                current.push_str(rest);
                break;
            }
            if has_words {
                // Start a new line and try again.
                lines.push(std::mem::replace(&mut current, " ".repeat(hang)));
                prefix = hang;
                continue;
            }
            let (head, tail) = split_at_width(rest, room, true);
            current.push_str(head);
            lines.push(std::mem::replace(&mut current, " ".repeat(hang)));
            prefix = hang;
            rest = tail;
        }
    }
    if text_width(&current) > prefix || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// `text` (whitespace collapsed) broken into lines of at most `width` cells at word
/// boundaries. Past `max_lines` the rest is cut and the last line ends with an
/// ellipsis, so the cause at the end of a long error is the only loss.
pub fn wrap_words(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines = wrap_line(&flat, width);
    if lines.len() > max_lines.max(1) {
        lines.truncate(max_lines.max(1));
        if let Some(last) = lines.last_mut() {
            let mut head = split_at_width(last, width.saturating_sub(1), false)
                .0
                .to_string();
            head.push(ELLIPSIS);
            *last = head;
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
            Some(line) if text_width(line) + SEP.len() + text_width(part) <= width => {
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
    let needed: usize = full.iter().map(|t| text_width(t) + 2).sum::<usize>() + full.len() - 1;
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
        keep.iter().map(|h| text_width(&h.1)).sum::<usize>() + 2 * keep.len().saturating_sub(1)
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
