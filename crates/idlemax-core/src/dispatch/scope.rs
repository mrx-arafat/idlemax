//! Domain: a pick's scope paths against the files its project tracks. The orchestrator never
//! sees the working tree, so a path it names may not exist. Such a scope is a guess: kept,
//! its other paths would still call the task's real files out of scope (live: `demo.py`
//! named next to `tests/` for a change in `calc/__init__.py`), so it becomes the whole
//! project, whose off-limits paths the check still holds. Pure.

use crate::worker::checks::{in_scope, under};

/// The scope of a whole project: `**` matches every path (gitignore rules).
pub const WHOLE_PROJECT: &str = "**";
/// The note a pick's reason gets when its scope became the whole project.
pub const SCOPE_WIDENED: &str = "scope widened: the named paths did not exist";

/// Stars a scope pattern may hold; more is dropped, whatever the matcher's speed.
pub const MAX_STARS: usize = 8;

/// A scope pattern that covers every file (`*`, `**`, `*/**`, empty) disables the scope
/// check, so the pass may not name one; idlemax alone widens a scope, to `WHOLE_PROJECT`.
pub fn too_broad(pattern: &str) -> bool {
    pattern
        .split('/')
        .all(|part| matches!(part, "" | "." | "*" | "**"))
        || pattern.matches('*').count() > MAX_STARS
}

/// Takes the over-broad patterns out of `paths` and returns them.
pub fn drop_broad(paths: &mut Vec<String>) -> Vec<String> {
    let (broad, kept) = std::mem::take(paths)
        .into_iter()
        .partition(|p| too_broad(p));
    *paths = kept;
    broad
}

/// Top-level folders that hold a project's tests; the one a project has joins every scope.
const TEST_FOLDERS: [&str; 4] = ["tests", "test", "spec", "__tests__"];

/// The named paths when each matches a tracked file or is a new file in a folder the project
/// has, plus the files `task_text` names whose folder exists and the project's test folder,
/// none under `off_limits`; else None: the scope is widened. An empty file list means it is
/// unknown, and the named paths stay.
pub fn narrow_scope(
    named: &[String],
    files: &[String],
    task_text: &str,
    off_limits: &[String],
) -> Option<Vec<String>> {
    if files.is_empty() {
        return Some(named.to_vec());
    }
    let folder_exists = |p: &str| {
        p.rsplit_once('/')
            .is_some_and(|(dir, _)| files.iter().any(|f| f.starts_with(&format!("{dir}/"))))
    };
    let real = |n: &String| {
        files.iter().any(|f| in_scope(f, n)) || (!n.contains(['*', '?']) && folder_exists(n))
    };
    if !named.iter().all(real) {
        return None;
    }
    let mut kept = named.to_vec();
    let allowed = |p: &str| !off_limits.iter().any(|o| under(p, o));
    for p in paths_named(task_text) {
        let covered = kept.iter().any(|k| in_scope(&p, k));
        if !covered && allowed(&p) && (files.contains(&p) || folder_exists(&p)) {
            kept.push(p);
        }
    }
    for dir in TEST_FOLDERS {
        let probe = format!("{dir}/new_test");
        let used = files.iter().any(|f| f.starts_with(&format!("{dir}/")));
        if used && allowed(&probe) && !kept.iter().any(|k| in_scope(&probe, k)) {
            kept.push(format!("{dir}/"));
        }
    }
    Some(kept)
}

/// The relative file paths a task's text names, such as `seqs/pascal.py`: a word with a `/`
/// whose last part has an extension, never absolute and never with `..`.
fn paths_named(text: &str) -> Vec<String> {
    let path_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !path_char(c)) {
        let word = word.trim_end_matches('.');
        let file = word.rsplit('/').next().unwrap_or("");
        let ok = word.contains('/')
            && !word.starts_with('/')
            && !word.split('/').any(|p| p == ".." || p.is_empty())
            && file.contains('.')
            && !file.starts_with('.');
        if ok && !out.iter().any(|o| o == word) {
            out.push(word.to_string());
        }
    }
    out
}
