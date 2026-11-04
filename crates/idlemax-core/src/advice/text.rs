//! Domain: small text helpers for advice titles and keys.

use crate::types::{QuotaWindowKind, UnixMs};

pub const MINUTE_MS: i64 = 60_000;
pub const HOUR_MS: i64 = 60 * MINUTE_MS;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// "45 m", "1 h 40 m", "3 d 2 h".
pub fn duration(ms: i64) -> String {
    let mins = ms.max(0) / MINUTE_MS;
    let (d, h, m) = (mins / 1440, mins / 60 % 24, mins % 60);
    match (d, h) {
        (0, 0) => format!("{m} m"),
        (0, _) if m == 0 => format!("{h} h"),
        (0, _) => format!("{h} h {m} m"),
        _ if h == 0 => format!("{d} d"),
        _ => format!("{d} d {h} h"),
    }
}

/// At most `max` chars, with "..." when cut.
pub fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

/// Lowercase ascii words joined by dashes, at most 60 chars.
pub fn slug(s: &str) -> String {
    let words: Vec<String> = s
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    clip(&words.join("-"), 60).trim_end_matches('.').to_string()
}

/// UTC day number, for keys of advice that may recur once per day.
pub fn day(now_ms: UnixMs) -> i64 {
    now_ms.div_euclid(DAY_MS)
}

pub fn window_label(kind: Option<QuotaWindowKind>) -> String {
    match kind {
        Some(QuotaWindowKind::Rolling { minutes }) if minutes % 60 == 0 => {
            format!("{} h window", minutes / 60)
        }
        Some(QuotaWindowKind::Rolling { minutes }) => format!("{minutes} min window"),
        Some(QuotaWindowKind::Weekly) => "weekly window".into(),
        None => "quota".into(),
    }
}
