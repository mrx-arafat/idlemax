//! Domain: naming conventions for branches, worktrees, panes and tabs. Pure: no I/O, no clock.

use std::path::{Path, PathBuf};

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const MAX_PANE_CHARS: usize = 32;
const MAX_PROJECT_PANE_CHARS: usize = 10;
const MAX_TASK_SLUG_PREFIX: usize = 32;

/// Lowercase alphanumeric slug with runs of non-alphanumeric chars collapsed to `-`.
/// No `-` at either end.
pub fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// 64-bit FNV-1a hash.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// First 32 chars of slugged task id, a `-`, and first 6 hex digits of FNV-1a hash of whole id.
///
/// If cutting at 32 chars leaves a trailing hyphen, it is trimmed to avoid a double hyphen
/// (`--`) before the hash. If the slugged id is empty (e.g. an all-symbol task id), only the
/// 6-digit hex hash is returned with no leading or trailing hyphen.
pub fn task_slug(task_id: &str) -> String {
    let s = slug(task_id);
    let prefix = if s.len() > MAX_TASK_SLUG_PREFIX {
        &s[..MAX_TASK_SLUG_PREFIX]
    } else {
        &s
    };
    let prefix = prefix.trim_end_matches('-');
    let hash = fnv1a_64(task_id.as_bytes());
    let hex = format!("{hash:016x}");
    let hash_prefix = &hex[..6];
    if prefix.is_empty() {
        hash_prefix.to_string()
    } else {
        format!("{prefix}-{hash_prefix}")
    }
}

/// Base-36 representation of a 64-bit integer (lowercase 0-9, a-z).
pub fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    // Safety / pure: all bytes are ascii digits or lowercase letters.
    String::from_utf8(buf).unwrap_or_default()
}

/// Pane name: `im-<project>-<task>-<run>`.
/// Matches `[a-z][a-z0-9_-]{0,31}`.
pub fn pane_name(project: &str, task_id: &str, run_id: u64) -> String {
    let p_slug = slug(project);
    let t_slug = task_slug(task_id);
    pane_name_from_slugs(&p_slug, &t_slug, run_id)
}

/// Safely cuts a string to at most `n` characters without splitting char boundaries.
fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Builds pane name from pre-computed slugs and run id.
pub fn pane_name_from_slugs(project_slug: &str, task_slug: &str, run_id: u64) -> String {
    let p_cut = take_chars(project_slug, MAX_PROJECT_PANE_CHARS);
    let r_str = base36(run_id);
    let prefix = format!("im-{p_cut}-");
    let suffix = format!("-{r_str}");
    let fixed_len = prefix.len() + suffix.len();
    let budget = MAX_PANE_CHARS.saturating_sub(fixed_len);
    let t_cut = take_chars(task_slug, budget);
    format!("{prefix}{t_cut}{suffix}")
}

/// Herdr tab label: `im: <Project>` with project name as written.
pub fn tab_label(project: &str) -> String {
    format!("im: {project}")
}

/// Git branch name: `idlemax/<task_slug>`, with `-r<run_id>` when tried again.
pub fn branch_name(task_slug: &str, run_id: Option<u64>) -> String {
    match run_id {
        Some(r) => format!("idlemax/{task_slug}-r{r}"),
        None => format!("idlemax/{task_slug}"),
    }
}

/// Worktree directory: `$IDLEMAX_HOME/worktrees/<project_slug>/<task_slug>[-r<run_id>]`.
pub fn worktree_dir(
    worktrees_dir: &Path,
    project_slug: &str,
    task_slug: &str,
    run_id: Option<u64>,
) -> PathBuf {
    let leaf = match run_id {
        Some(r) => format!("{task_slug}-r{r}"),
        None => task_slug.to_string(),
    };
    worktrees_dir.join(project_slug).join(leaf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rules() {
        assert_eq!(slug("Hello World!"), "hello-world");
        assert_eq!(slug("---a--b---c---"), "a-b-c");
        assert_eq!(
            slug("github_issue:owner/repo#123"),
            "github-issue-owner-repo-123"
        );
        assert_eq!(slug("   ***   "), "");
        assert_eq!(slug("already-clean-123"), "already-clean-123");
    }

    #[test]
    fn base36_values() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        assert_eq!(base36(1_000_000), "lfls");
    }

    #[test]
    fn task_slug_differs_when_first_40_chars_match() {
        let prefix = "github_issue:owner/repo#1234567890123456";
        assert!(prefix.len() >= 40);
        let id1 = format!("{prefix}-fix-memory-leak");
        let id2 = format!("{prefix}-fix-network-drop");
        let s1 = task_slug(&id1);
        let s2 = task_slug(&id2);
        assert_ne!(s1, s2);
        assert_eq!(s1.len(), 32 + 1 + 6);
    }

    #[test]
    fn pane_name_matches_herdr_rule_and_stays_within_32_chars() {
        let long_project = "a-very-long-project-name-that-exceeds-ten-characters";
        let task_id = "task with spaces & symbols !@#$ and very long title 123456789";
        let run_id = 1_500_000;

        let name1 = pane_name(long_project, task_id, run_id);
        let name2 = pane_name(long_project, task_id, run_id + 1);

        assert_ne!(name1, name2);
        assert!(name1.len() <= 32, "len {} > 32: {}", name1.len(), name1);
        assert!(name2.len() <= 32, "len {} > 32: {}", name2.len(), name2);

        for name in [&name1, &name2] {
            let first = name.chars().next().unwrap();
            assert!(
                first.is_ascii_lowercase(),
                "first char not lowercase: {name}"
            );
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'),
                "name contains invalid chars: {name}"
            );
        }
    }

    #[test]
    fn tab_label_formats_as_specified() {
        assert_eq!(tab_label("IdleMax"), "im: IdleMax");
    }

    #[test]
    fn branch_and_worktree_paths_with_and_without_run_id() {
        let wt = Path::new("/state/worktrees");
        assert_eq!(branch_name("task-abc", None), "idlemax/task-abc");
        assert_eq!(branch_name("task-abc", Some(42)), "idlemax/task-abc-r42");

        assert_eq!(
            worktree_dir(wt, "proj", "task-abc", None),
            PathBuf::from("/state/worktrees/proj/task-abc")
        );
        assert_eq!(
            worktree_dir(wt, "proj", "task-abc", Some(42)),
            PathBuf::from("/state/worktrees/proj/task-abc-r42")
        );
    }

    #[test]
    fn pane_name_from_slugs_unicode_safety() {
        // Multi-byte chars should not cause char boundary panics when sliced.
        let name = pane_name_from_slugs("🦀🦀🦀🦀🦀🦀", "🚀🚀🚀🚀🚀🚀", 123);
        assert!(name.starts_with("im-"));
    }
}
