//! Domain: one line on why a pass's picks were dropped, for the plan and the log. Pure.

/// Reasons a summary names at most; the rest are counted.
const MAX_REASONS: usize = 3;

/// `23 picks dropped: account max may not be spent: resting (23)`, the most common reasons
/// first; None when nothing was dropped. Each entry is `<task>: <why>` as `validate` writes.
pub fn dropped_summary(dropped: &[String]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    // (why, count) in order of first appearance, so equal counts keep the pass's order.
    let mut reasons: Vec<(&str, usize)> = Vec::new();
    for entry in dropped {
        let why = entry
            .split_once(": ")
            .map_or(entry.as_str(), |(_, why)| why);
        match reasons.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => reasons.push((why, 1)),
        }
    }
    reasons.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let mut parts: Vec<String> = reasons
        .iter()
        .take(MAX_REASONS)
        .map(|(why, n)| format!("{why} ({n})"))
        .collect();
    let more = reasons.len().saturating_sub(MAX_REASONS);
    if more > 0 {
        parts.push(format!(
            "{more} more reason{}",
            if more == 1 { "" } else { "s" }
        ));
    }
    Some(format!(
        "{} picks dropped: {}",
        dropped.len(),
        parts.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_the_most_common_reasons_with_their_counts() {
        assert_eq!(dropped_summary(&[]), None);
        let mut all: Vec<String> = (0..21)
            .map(|i| format!("roadmap:3:t{i}: account max may not be spent: resting"))
            .collect();
        all.push("roadmap:4:x: model gpt was not offered for max".into());
        all.push("roadmap:4:y: model gpt was not offered for max".into());
        all.push("roadmap:5:z: not an offered task".into());
        all.push("roadmap:5:w: picked twice".into());
        assert_eq!(
            dropped_summary(&all).unwrap(),
            "25 picks dropped: account max may not be spent: resting (21); \
             model gpt was not offered for max (2); not an offered task (1); 1 more reason"
        );
    }
}
