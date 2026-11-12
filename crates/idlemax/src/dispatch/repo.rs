//! Adapter: git facts of one folder, read with `git --no-optional-locks` so no read takes a
//! lock in the owner's repository, and with `safe_git::SAFE`, so no git here runs a hook or
//! an fsmonitor command: one started from the agent would run outside the allowlist, the
//! lines and the kill switch. Every call has a 5 s timeout; a failed call leaves its
//! field empty.

use std::path::{Path, PathBuf};
use std::time::Duration;

use idlemax_core::dispatch::inputs::Checkout;
use tokio::process::Command;

const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// The roadmap files tried, in order, on the base branch.
const ROADMAP_FILES: [&str; 2] = ["ROADMAP.md", "docs/ROADMAP.md"];

/// Runs `git --no-optional-locks -C <dir> <args>`; stdout on success.
pub(crate) async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    git_result(dir, args).await.ok()
}

/// Like `git`, with stderr as the error.
async fn git_result(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("--no-optional-locks")
        .args(crate::safe_git::SAFE)
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().chars().take(300).collect())
    }
}

/// Makes or moves `dir`, a detached worktree of `project` at `commit`. A folder that is not
/// this project's checkout is left alone.
pub async fn prepare_checkout(project: &Path, dir: &Path, commit: &str) -> Checkout {
    let mut out = checkout_state(project, dir).await;
    if out.error.is_some() || out.commit.as_deref() == Some(commit) {
        return out;
    }
    let shown = dir.to_string_lossy().into_owned();
    let result = if dir.exists() {
        git_result(dir, &["checkout", "-q", "--detach", commit]).await
    } else {
        if let Some(parent) = dir.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                out.error = Some(format!("cannot create {}: {e}", parent.display()));
                return out;
            }
        }
        let add = ["worktree", "add", "-q", "--detach", shown.as_str(), commit];
        git_result(project, &add).await
    };
    match result {
        Ok(_) => checkout_state(project, dir).await,
        Err(e) => Checkout {
            error: Some(e),
            ..out
        },
    }
}

/// Where the checkout is now, without changing it.
pub async fn checkout_state(project: &Path, dir: &Path) -> Checkout {
    let path = dir.to_string_lossy().into_owned();
    let mut out = Checkout {
        path: path.clone(),
        commit: None,
        error: None,
    };
    if !dir.exists() {
        return out;
    }
    let common = |d: &Path| {
        let d = d.to_path_buf();
        async move {
            let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
            git(&d, &args).await.map(|c| canonical(Path::new(c.trim())))
        }
    };
    if common(dir).await.is_none() || common(dir).await != common(project).await {
        out.error = Some(format!("not this project's checkout: remove {path}"));
        return out;
    }
    out.commit = git(dir, &["rev-parse", "HEAD"])
        .await
        .map(|c| c.trim().to_string());
    out
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The main working tree of the repository holding `dir`: the parent of its common git
/// directory when that is named `.git`. None for anything else (a bare repository).
pub async fn root(dir: &Path) -> Option<PathBuf> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let common = PathBuf::from(common.trim());
    (common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
}

/// owner/name of the origin remote, when it is on GitHub.
pub async fn slug(root: &Path) -> Option<String> {
    let url = git(root, &["remote", "get-url", "origin"]).await?;
    github_slug(url.trim())
}

/// `git@github.com:o/n.git` or `https://github.com/o/n` as `o/n`.
pub fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = rest.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

/// The local branch `origin/HEAD` names, kept only when it exists locally: fetching is out of
/// scope, so a remote-tracking ref would be a stale base.
pub async fn default_branch(root: &Path) -> Option<String> {
    let full = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await?;
    let name = full.trim().strip_prefix("origin/")?.to_string();
    let local = format!("refs/heads/{name}");
    git(root, &["show-ref", "--verify", "--quiet", &local]).await?;
    Some(name)
}

/// The branch checked out now; None for a detached HEAD.
pub async fn current_branch(root: &Path) -> Option<String> {
    let name = git(root, &["symbolic-ref", "--short", "-q", "HEAD"]).await?;
    Some(name.trim().to_string()).filter(|n| !n.is_empty())
}

/// The commit at the tip of the local base branch.
pub async fn base_commit(root: &Path, base: &str) -> Option<String> {
    let spec = format!("refs/heads/{base}^{{commit}}");
    let out = git(root, &["rev-parse", "--verify", "--quiet", &spec]).await?;
    Some(out.trim().to_string()).filter(|c| !c.is_empty())
}

/// The roadmap text at `rev` (`refs/heads/<base>`, or `HEAD`): `ROADMAP.md`, then
/// `docs/ROADMAP.md`.
pub async fn roadmap_text(root: &Path, rev: &str) -> Option<String> {
    for file in ROADMAP_FILES {
        if let Some(text) = git(root, &["show", &format!("{rev}:{file}")]).await {
            return Some(text);
        }
    }
    None
}

/// The start of AGENTS.md, else CLAUDE.md, at `rev`: at most `max` characters, with no
/// control character but newlines and tabs.
pub async fn conventions(root: &Path, rev: &str, max: usize) -> Option<String> {
    for file in ["AGENTS.md", "CLAUDE.md"] {
        if let Some(text) = git(root, &["show", &format!("{rev}:{file}")]).await {
            let clean = text
                .chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'));
            return Some(clean.take(max).collect());
        }
    }
    None
}

/// The files tracked at `rev`, from `git ls-tree -r --name-only -z`.
pub async fn tracked_files(root: &Path, rev: &str) -> Vec<String> {
    git(root, &["ls-tree", "-r", "--name-only", "-z", rev])
        .await
        .map(|out| {
            out.split('\0')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The paths changed in the working tree, from `git status --porcelain=v1 -z`.
pub async fn touched(root: &Path) -> Vec<String> {
    git(root, &["status", "--porcelain=v1", "-z"])
        .await
        .map(|out| parse_status(&out))
        .unwrap_or_default()
}

/// Each entry is `XY path`; a rename or copy is followed by its old path, which is skipped.
pub fn parse_status(out: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        paths.push(path.to_string());
        if code.contains('R') || code.contains('C') {
            entries.next();
        }
    }
    paths
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// git with a fixed configuration, so the developer's settings cannot change a test.
    pub(crate) fn git_cmd(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=idlemax test",
                "-c",
                "user.email=test@idlemax.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} in {}", dir.display());
    }

    /// A repository with one commit on `main`.
    pub(crate) fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_cmd(dir, &["init", "-q"]);
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        git_cmd(dir, &["add", "README.md"]);
        git_cmd(dir, &["commit", "-q", "-m", "first"]);
    }

    #[test]
    fn slugs_and_status_entries_parse() {
        assert_eq!(
            github_slug("git@github.com:o/n.git").as_deref(),
            Some("o/n")
        );
        assert_eq!(
            github_slug("https://github.com/o/n").as_deref(),
            Some("o/n")
        );
        assert_eq!(github_slug("https://gitlab.com/o/n"), None);
        let out = " M a.rs\0R  new.rs\0old.rs\0?? b/\0";
        assert_eq!(parse_status(out), ["a.rs", "new.rs", "b/"]);
    }

    #[tokio::test]
    async fn the_default_branch_is_the_local_branch_origin_head_names() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        repo(&origin);
        let clone = dir.path().join("clone");
        git_cmd(
            dir.path(),
            &["clone", "-q", &origin.to_string_lossy(), "clone"],
        );
        std::fs::write(clone.join("x"), "x").unwrap();
        git_cmd(&clone, &["add", "x"]);
        git_cmd(&clone, &["commit", "-q", "-m", "ahead"]);
        let root = root(&clone).await.unwrap();
        assert_eq!(root.canonicalize().unwrap(), clone.canonicalize().unwrap());
        assert_eq!(default_branch(&clone).await.as_deref(), Some("main"));
        let head = git(&clone, &["rev-parse", "HEAD"]).await.unwrap();
        assert_eq!(base_commit(&clone, "main").await, Some(head.trim().into()));

        git_cmd(&clone, &["checkout", "-q", "-b", "other"]);
        git_cmd(&clone, &["branch", "-q", "-D", "main"]);
        assert_eq!(default_branch(&clone).await, None);
    }
}
