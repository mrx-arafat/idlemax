//! Adapter: identifies idlemax's own worker sessions to exclude them from interactive use.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use idlemax_core::types::UnixMs;

use super::{ProbeDirs, TAIL_START_BYTES};

#[cfg(test)]
mod tests;

/// Maximum number of session files inspected per CLI before reporting the newest file.
pub const OWN_SESSION_WALK: usize = 64;

/// Bound on the number of cached session path classifications.
const MAX_CACHE_ENTRIES: usize = 1024;

/// (cli name, newest session file mtime) for Claude, Codex and agy, leaving out idlemax's
/// own worker sessions.
pub fn interactive_use(
    dirs: &ProbeDirs,
    own_sessions: &mut OwnSessions,
    now_ms: UnixMs,
) -> Vec<(String, UnixMs)> {
    let sources = [
        (
            "claude",
            dirs.claude_dir.as_ref().map(|d| d.join("projects")),
        ),
        ("codex", dirs.codex_dir.as_ref().map(|d| d.join("sessions"))),
        ("agy", dirs.agy_dir.as_ref().map(|d| d.join("brain"))),
    ];
    let mut out = Vec::new();
    for (cli, dir) in sources {
        let Some(dir) = dir else { continue };
        match super::jsonl_files(&dir) {
            Ok(files) => {
                if let Some(t) =
                    find_interactive_mtime(cli, files, dirs.worktrees_dir.as_deref(), own_sessions)
                {
                    out.push((cli.to_string(), t.min(now_ms)));
                }
            }
            Err(e) => tracing::debug!(cli, "interactive use: {e}"),
        }
    }
    out
}

/// Walks session files newest first, returning the mtime of the first non-worker session.
/// If all inspected files up to `OWN_SESSION_WALK` are workers, returns the newest
/// file's mtime if there were more files than the walk bound, or None otherwise.
pub fn find_interactive_mtime(
    cli: &str,
    mut files: Vec<(UnixMs, PathBuf)>,
    worktrees_dir: Option<&Path>,
    own_sessions: &mut OwnSessions,
) -> Option<UnixMs> {
    if files.is_empty() {
        return None;
    }
    files.sort_unstable_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    let newest_mtime = files.first().map(|(t, _)| *t);

    for (mtime, path) in files.iter().take(OWN_SESSION_WALK) {
        if !own_sessions.is_worker(cli, path, worktrees_dir) {
            return Some(*mtime);
        }
    }

    if files.len() > OWN_SESSION_WALK {
        newest_mtime
    } else {
        None
    }
}

/// Remembers whether session files belong to idlemax workers to avoid re-reading tails on every sample.
#[derive(Debug, Clone, Default)]
pub struct OwnSessions {
    cache: HashMap<PathBuf, bool>,
}

impl OwnSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks whether `path` is an idlemax worker session.
    ///
    /// For Claude, a session is a worker session if the last record in its tail that carries `cwd`
    /// is located under `worktrees_dir`. For other CLIs (Codex, agy), worker folder locations are
    /// not yet proven, so they are never classified as worker sessions.
    ///
    /// Only definite answers (where a `cwd` record was found) are cached.
    /// Missing files, read errors, or session files without a `cwd` record yet are not cached.
    pub fn is_worker(&mut self, cli: &str, path: &Path, worktrees_dir: Option<&Path>) -> bool {
        if cli != "claude" {
            return false;
        }
        let Some(worktrees_dir) = worktrees_dir else {
            return false;
        };
        if let Some(&is_w) = self.cache.get(path) {
            return is_w;
        }

        match check_file_is_worker(path, worktrees_dir) {
            Some(is_w) => {
                if self.cache.len() >= MAX_CACHE_ENTRIES {
                    self.cache.clear();
                }
                self.cache.insert(path.to_path_buf(), is_w);
                is_w
            }
            None => false,
        }
    }
}

fn check_file_is_worker(path: &Path, worktrees_dir: &Path) -> Option<bool> {
    let tail = read_tail(path, TAIL_START_BYTES).ok()?;
    let cwd = extract_last_cwd(&tail)?;
    Some(is_under(&cwd, worktrees_dir))
}

fn read_tail(path: &Path, max: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(match (start, text.find('\n')) {
        (0, _) | (_, None) => text,
        (_, Some(i)) => text[i + 1..].to_string(),
    })
}

fn extract_last_cwd(tail: &str) -> Option<PathBuf> {
    tail.lines()
        .rev()
        .filter(|line| line.contains("\"cwd\""))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|rec| {
            rec.get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
        })
}

fn is_under(path: &Path, base: &Path) -> bool {
    match (path.canonicalize(), base.canonicalize()) {
        (Ok(p), Ok(b)) => p.starts_with(&b),
        _ => path.starts_with(base),
    }
}
