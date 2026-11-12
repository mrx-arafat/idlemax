//! Domain: roadmap items of a project (D7 source 2): a Markdown heading of level 2 to 4 whose
//! section has a "Done when" line. Pure.

use serde::{Deserialize, Serialize};

/// Longest done-when text kept, in characters.
pub const MAX_DONE_WHEN_CHARS: usize = 600;

/// One roadmap item.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoadmapItem {
    pub title: String,
    /// What proves the item done, from its "Done when" line and the lines that continue it.
    pub done_when: String,
    /// The heading ends with "(done)".
    pub done: bool,
    /// The heading or the section says it needs the owner.
    pub owner: bool,
}

/// Every item of a roadmap, in file order. A heading without a done-when line is not an item.
pub fn parse_roadmap(text: &str) -> Vec<RoadmapItem> {
    let mut items = Vec::new();
    let mut section: Option<(String, Vec<&str>)> = None;
    let mut fenced = false;
    for line in text.lines() {
        let fence = line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
        fenced ^= fence;
        // A `#` line inside a code block is code, not a heading.
        if let Some(title) = heading(line).filter(|_| !fenced && !fence) {
            items.extend(section.take().and_then(|(t, lines)| item(&t, &lines)));
            section = Some((title, Vec::new()));
        } else if let Some((_, lines)) = section.as_mut() {
            lines.push(line);
        }
    }
    items.extend(section.and_then(|(t, lines)| item(&t, &lines)));
    items
}

/// The text of a level 2 to 4 heading.
fn heading(line: &str) -> Option<String> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    let rest = line.get(hashes..)?;
    ((2..=4).contains(&hashes) && rest.starts_with(' ')).then(|| plain(rest.trim()))
}

fn item(title: &str, lines: &[&str]) -> Option<RoadmapItem> {
    let start = lines.iter().position(|l| done_when_rest(l).is_some())?;
    let mut parts = vec![done_when_rest(lines[start])?];
    for next in &lines[start + 1..] {
        let continues = next.starts_with([' ', '\t']) && !list_marker(next.trim_start());
        if !continues || next.trim().is_empty() {
            break;
        }
        parts.push(next.trim().to_string());
    }
    let done_when: String = plain(&parts.join(" "))
        .chars()
        .take(MAX_DONE_WHEN_CHARS)
        .collect();
    let mentions_owner = |s: &str| {
        let s = s.to_lowercase();
        s.contains("needs the owner") || s.contains("owner's ok")
    };
    Some(RoadmapItem {
        title: title.to_string(),
        done_when,
        done: title.to_lowercase().ends_with("(done)"),
        owner: mentions_owner(title) || lines.iter().any(|l| mentions_owner(l)),
    })
}

/// The rest of a line that starts with "Done when" once list markers and emphasis are gone.
fn done_when_rest(line: &str) -> Option<String> {
    let mut s = line.trim_start();
    if list_marker(s) {
        s = s[1..].trim_start();
    }
    let s = s.replace(['*', '_'], "");
    let rest = s.strip_prefix("Done when")?;
    // "Done whenever" is not the marker: a colon, a space or the end must follow.
    if rest.starts_with(|c: char| c.is_alphanumeric()) {
        return None;
    }
    Some(rest.trim_start_matches(':').trim().to_string())
}

fn list_marker(s: &str) -> bool {
    s.starts_with("- ") || s.starts_with("* ") || s.starts_with("+ ")
}

/// Control characters become spaces: these strings reach the CLI tables and the AI prompt.
fn plain(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_headings_with_a_done_when_line() {
        let text = "# Roadmap\n\n\
### 1. Verify the agent\n\n\
- **Why:** it is new.\n\
- **Done when:** The checks pass on the server,\n  \
and its sites show no slowdown\n  \
during the run.\n\
- **Other:** x\n\n\
### 2. Just notes\n\n\
Nothing to prove here.\n\n\
#### 3. Show capacity (done)\n\n\
Done when: each view answers it.\n";
        let items = parse_roadmap(text);
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[0].title, "1. Verify the agent");
        assert_eq!(
            items[0].done_when,
            "The checks pass on the server, and its sites show no slowdown during the run."
        );
        assert!(!items[0].done && !items[0].owner);
        assert!(items[1].done);
        assert_eq!(items[1].done_when, "each view answers it.");
    }

    #[test]
    fn code_fences_and_look_alike_words_are_not_items() {
        let text = "## Real\n\n```sh\n## not a heading\n```\nDone when: it ships\n\n## Fake\n\nDone whenever you like\n";
        let items = parse_roadmap(text);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(
            (items[0].title.as_str(), items[0].done_when.as_str()),
            ("Real", "it ships")
        );
    }

    #[test]
    fn owner_items_are_marked_and_control_characters_become_spaces() {
        let text = "## Ship\u{1b}it\n\nThis needs the owner's OK.\n\nDone when: it\u{7}ships\n";
        let items = parse_roadmap(text);
        assert_eq!(items[0].title, "Ship it");
        assert_eq!(items[0].done_when, "it ships");
        assert!(items[0].owner);
    }
}
