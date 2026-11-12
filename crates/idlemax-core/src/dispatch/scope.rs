//! Domain: a pick's scope paths against the files its project tracks. The orchestrator never
//! sees the working tree, so a path it names may not exist. Such a scope is a guess: kept,
//! its other paths would still call the task's real files out of scope (live: `demo.py`
//! named next to `tests/` for a change in `calc/__init__.py`), so it becomes the whole
//! project, whose off-limits paths the check still holds. Pure.

use crate::worker::checks::in_scope;

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

/// The named paths when each matches a tracked file, else None: the scope is widened. An
/// empty file list means it is unknown, and the named paths stay.
pub fn narrow_scope(named: &[String], files: &[String]) -> Option<Vec<String>> {
    let real = |n: &String| files.iter().any(|f| in_scope(f, n));
    (files.is_empty() || named.iter().all(real)).then(|| named.to_vec())
}
