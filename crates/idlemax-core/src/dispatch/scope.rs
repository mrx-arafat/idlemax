//! Domain: a pick's scope paths against the files its project tracks. The scope is a fence
//! (D8): the orchestrator never sees the working tree, so a path it names may not exist;
//! such a path is dropped alone, and a pick with no real path left is not used. A scope is
//! never widened to the whole project. Pure.

use super::plan::{PassPick, PlanInput};
use super::project::relative_path_problem;
use super::task::{Task, TaskSource};
use crate::worker::checks::{in_scope, under};

/// Stars a scope pattern may hold; more is dropped, whatever the matcher's speed.
pub const MAX_STARS: usize = 8;

/// A scope pattern that covers every file (`*`, `**`, `*/**`, empty) disables the scope
/// check, so the pass may not name one.
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

/// A pick's scope: the paths kept, and the named ones dropped because they do not exist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scope {
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
}

/// The pick's scope and its reason, with a note when paths were dropped; or why the pick
/// cannot be used: none of its scope paths exist.
pub fn pick_scope(input: &PlanInput<'_>, task: &Task, pp: &PassPick) -> Result<Scope, String> {
    let project = input
        .projects
        .iter()
        .find(|v| v.project.id == task.project_id);
    let repo = project.and_then(|v| {
        let p = &v.project;
        input
            .repos
            .iter()
            .find(|r| r.device == p.device && r.repo.path == p.path)
    });
    let files = repo.map_or(&[][..], |r| r.repo.files.as_slice());
    let complete = repo.is_none_or(|r| r.repo.files_cut == 0);
    let detail = repo
        .and_then(|r| r.repo.roadmap.iter().find(|i| i.title == task.title))
        .map_or("", |i| i.detail.as_str());
    let done = task.done_check.as_deref().unwrap_or("");
    // Only text the owner wrote adds scope: never an issue title or session text.
    let owners = matches!(task.source, TaskSource::Roadmap | TaskSource::BrokenCheck);
    let text = match owners {
        true => format!("{} {detail} {done}", task.title),
        false => String::new(),
    };
    let off_limits = project.map_or_else(Vec::new, |v| v.project.settings.all_off_limits());
    narrow_scope(
        &pp.scope_paths,
        &Files { files, complete },
        &text,
        &off_limits,
    )
}

/// The tracked files a project reported, and whether that is all of them: a list cut at
/// `MAX_FILES` cannot prove a path missing.
pub struct Files<'a> {
    pub files: &'a [String],
    pub complete: bool,
}

/// The named paths that match a tracked file or are a new file in a folder the project has,
/// plus the files `task_text` names whose folder exists and the project's test folder, none
/// under `off_limits`. Named paths that do not exist are dropped; with none left, Err. An
/// empty file list means it is unknown, and the named paths stay; on a cut list a missing
/// path is unknown, and it stays when it is a safe relative path.
pub fn narrow_scope(
    named: &[String],
    listed: &Files<'_>,
    task_text: &str,
    off_limits: &[String],
) -> Result<Scope, String> {
    let files = listed.files;
    if files.is_empty() {
        return Ok(Scope {
            kept: named.to_vec(),
            dropped: Vec::new(),
        });
    }
    let folder_exists = |p: &str| {
        p.rsplit_once('/')
            .is_some_and(|(dir, _)| files.iter().any(|f| f.starts_with(&format!("{dir}/"))))
    };
    let unknown = |n: &str| !listed.complete && relative_path_problem(n).is_ok() && !too_broad(n);
    let real = |n: &String| {
        files.iter().any(|f| in_scope(f, n))
            || (!n.contains(['*', '?']) && folder_exists(n))
            || unknown(n)
    };
    let (mut kept, dropped): (Vec<String>, Vec<String>) = named.iter().cloned().partition(real);
    if kept.is_empty() && !dropped.is_empty() {
        return Err(format!(
            "none of its scope paths exist: {}",
            dropped.join(", ")
        ));
    }
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
    Ok(Scope { kept, dropped })
}

/// The relative file paths a task's text names, such as `seqs/pascal.py`: a word with a `/`
/// whose last part has an extension, never absolute, never with `..` and never in or of a
/// dot folder or file (`.github/`, `.claude/`, `.env`).
fn paths_named(text: &str) -> Vec<String> {
    let path_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !path_char(c)) {
        let word = word.trim_end_matches('.');
        let file = word.rsplit('/').next().unwrap_or("");
        let ok = word.contains('/')
            && !word.starts_with('/')
            && !word.split('/').any(|p| p.is_empty() || p.starts_with('.'))
            && file.contains('.');
        if ok && !out.iter().any(|o| o == word) {
            out.push(word.to_string());
        }
    }
    out
}
