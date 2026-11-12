//! Adapter: the owner's review of a finished task on disk (D16). Keep removes the worktree
//! folder and leaves the branch; keep with merge first merges the branch into the owner's
//! current branch, only when the owner's checkout is clean and the merge has no conflict;
//! discard removes the folder and then the branch. Every remove is git's safe one, without
//! force, so git refuses while work is left. Each refusal says why and what to do.

use std::path::{Path, PathBuf};
use std::time::Duration;

use std::collections::BTreeSet;
use std::sync::Arc;

use idlemax_core::job_run::RunClock;
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{Job, JobStatus, UnixMs};
use idlemax_core::worker::report::WorkerPhase;
use idlemax_core::worker::{WorkerAction, WorkerSpec};

use super::cmd::{Cmd, Commands};
use super::host::HostKind;
use super::steps::brief_path;
use super::steps::Progress;
use super::worktree::{delete_branch, remove_worktree};
use super::{new_tag, Phase, Tracked, Workers};

const GIT_LIMIT: Duration = Duration::from_secs(10);
const MERGE_LIMIT: Duration = Duration::from_secs(120);

/// Carries out `w.action` for its Run: what was done, or why nothing was.
pub fn review(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    w: &WorkerSpec,
) -> Result<String, String> {
    let folder = w
        .worktree
        .as_ref()
        .map(PathBuf::from)
        .ok_or("the Run has no worktree")?;
    let branch = w.branch.clone().ok_or("the Run has no branch")?;
    let repo = Path::new(&w.repo);
    match w.action {
        WorkerAction::Keep => keep(commands, worktrees_dir, repo, &folder, &branch),
        WorkerAction::KeepMerge => {
            let tip = checked_tip(commands, repo, &branch, w.tip.as_deref())?;
            merge(commands, repo, &branch, &tip)?;
            keep(commands, worktrees_dir, repo, &folder, &branch)
                .map(|done| format!("merged {branch} into your branch; {done}"))
        }
        WorkerAction::Discard => discard(commands, worktrees_dir, repo, &folder, &branch),
        WorkerAction::Launch | WorkerAction::Resume => Err("not a review action".into()),
    }
}

fn keep(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    repo: &Path,
    folder: &Path,
    branch: &str,
) -> Result<String, String> {
    remove_folder(commands, worktrees_dir, repo, folder)?;
    let _ = std::fs::remove_file(PathBuf::from(format!("{}.json", folder.display())));
    remove_side_files(folder);
    Ok(format!("kept {branch}; its worktree folder is removed"))
}

fn discard(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    repo: &Path,
    folder: &Path,
    branch: &str,
) -> Result<String, String> {
    remove_folder(commands, worktrees_dir, repo, folder)?;
    delete_branch(commands, worktrees_dir, repo, folder, branch)
        .map_err(|e| format!("the branch {branch} could not be deleted: {e}"))?;
    remove_side_files(folder);
    Ok(format!("discarded {branch} and its worktree"))
}

/// git's safe remove; a folder already gone is fine. A refusal names the files left.
fn remove_folder(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    repo: &Path,
    folder: &Path,
) -> Result<(), String> {
    if !folder.exists() {
        return Ok(());
    }
    remove_worktree(commands, worktrees_dir, repo, folder).map_err(|e| {
        let left = git(commands, folder, &["status", "--porcelain"], GIT_LIMIT)
            .map(|out| {
                out.lines()
                    .map(|l| l.get(3..).unwrap_or(l).to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        format!(
            "{} still holds {}; commit or delete them there, then run the command again ({e})",
            folder.display(),
            if left.is_empty() {
                "work".to_string()
            } else {
                left.join(", ")
            }
        )
    })
}

/// The commit the launch's check judged, while `refs/heads/<branch>` still points at it: a
/// branch moved since would merge work no check has seen.
fn checked_tip(
    commands: &dyn Commands,
    repo: &Path,
    branch: &str,
    tip: Option<&str>,
) -> Result<String, String> {
    let tip = tip.ok_or_else(|| {
        format!(
            "{branch} has no checked commit to merge; merge it by hand, or keep it without --merge"
        )
    })?;
    let full = format!("refs/heads/{branch}^{{commit}}");
    let now = git(
        commands,
        repo,
        &["rev-parse", "--verify", "-q", &full],
        GIT_LIMIT,
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    match now == tip {
        true => Ok(tip.to_string()),
        false => Err(format!(
            "{branch} has moved since its check; look at it and merge it by hand, or keep it without --merge"
        )),
    }
}

/// Merges `tip`, the checked commit of `branch`, into the owner's current branch, or says why
/// it will not: a checkout with uncommitted changes, a detached HEAD, a conflict, or a merge
/// that fails once started, which is aborted so the checkout is as it was.
fn merge(commands: &dyn Commands, repo: &Path, branch: &str, tip: &str) -> Result<(), String> {
    let shown = repo.display();
    let dirty = git(commands, repo, &["status", "--porcelain"], GIT_LIMIT)?;
    if !dirty.trim().is_empty() {
        return Err(format!(
            "your checkout {shown} has uncommitted changes; commit or stash them and run the merge again, or keep without --merge"
        ));
    }
    if git(commands, repo, &["symbolic-ref", "-q", "HEAD"], GIT_LIMIT).is_err() {
        return Err(format!(
            "your checkout {shown} is on a detached HEAD; check out a branch and run the merge again"
        ));
    }
    let probe = ["merge-tree", "--write-tree", "HEAD", tip];
    let tested = run(commands, repo, &probe, GIT_LIMIT)?;
    match tested.code {
        Some(0) => {}
        Some(1) => {
            return Err(format!(
                "{branch} conflicts with your branch; merge it by hand, or keep it without --merge"
            ))
        }
        _ => {
            return Err(
                "the merge test needs git 2.38 or newer (git merge-tree --write-tree)".into(),
            )
        }
    }
    let message = format!("Merge branch '{branch}'");
    let args = ["merge", "--no-edit", "-m", &message, tip];
    let merged = run(commands, repo, &args, MERGE_LIMIT);
    match merged {
        Ok(out) if out.code == Some(0) => Ok(()),
        failed => {
            let why = match failed {
                Ok(out) => out.stderr.trim().to_string(),
                Err(e) => e,
            };
            let _ = run(commands, repo, &["merge", "--abort"], GIT_LIMIT);
            let after =
                git(commands, repo, &["status", "--porcelain"], GIT_LIMIT).unwrap_or_default();
            let state = if after.trim().is_empty() {
                "it was aborted and your checkout is as it was"
            } else {
                "it was aborted, but your checkout still shows changes: look at git status"
            };
            Err(format!("the merge of {branch} failed ({why}); {state}"))
        }
    }
}

/// The brief and the review files beside the worktree.
fn remove_side_files(folder: &Path) {
    let _ = std::fs::remove_file(brief_path(folder));
    for name in ["review-schema.json", "review-answer.json"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}.{name}", folder.display())));
    }
}

fn git(
    commands: &dyn Commands,
    dir: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    let out = run(commands, dir, args, limit)?;
    match out.code {
        Some(0) => Ok(out.stdout),
        _ => Err(out.stderr.trim().to_string()),
    }
}

fn run(
    commands: &dyn Commands,
    dir: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<super::cmd::Output, String> {
    let mut all = vec!["-C".to_string(), dir.display().to_string()];
    all.extend(args.iter().map(|s| s.to_string()));
    let cmd = Cmd {
        program: "git".into(),
        args: all,
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit,
        throttle: false,
        tag: None,
    };
    commands
        .run(&cmd)
        .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
}

impl Workers {
    /// Runs the owner's keep, merge or discard off the manager's thread.
    pub(super) fn start_review(
        &mut self,
        job: Job,
        worker: WorkerSpec,
        now_ms: UnixMs,
        awake_ms: i64,
    ) -> Vec<JobUpdate> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (commands, dir, spec) = (
            Arc::clone(&self.commands),
            self.paths.worktrees_dir(),
            worker.clone(),
        );
        std::thread::spawn(move || {
            let _ = tx.send(review(commands.as_ref(), &dir, &spec));
        });
        // A review starts no agent, so its host is never asked.
        let tracked = Tracked {
            job,
            worker,
            host: Arc::clone(&self.host),
            kind: HostKind::Herdr,
            tag: new_tag(0, now_ms),
            clock: RunClock::start(now_ms, awake_ms),
            paused: false,
            progress: Progress::default(),
            members: Vec::new(),
            throttled: BTreeSet::new(),
            phase: Phase::Review { rx },
            saved: None,
        };
        let update = self.update(&tracked, JobStatus::Running, WorkerPhase::Preparing, now_ms);
        self.tracked.insert(tracked.job.id, tracked);
        vec![update]
    }
}

#[cfg(test)]
mod tests;
