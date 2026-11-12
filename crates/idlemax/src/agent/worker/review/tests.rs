//! Tests: keep, merge and discard in a temp repository with real git, kept off the
//! developer's config.

use std::path::{Path, PathBuf};

use idlemax_core::worker::{WorkerAction, WorkerSpec};

use super::super::cmd::{Cmd, CmdError, Commands, OsCommands, Output};
use super::super::worktree::{create_worktree, NewWorktree};
use super::review;

struct IsolatedGit(OsCommands);

impl Commands for IsolatedGit {
    fn run(&self, cmd: &Cmd) -> Result<Output, CmdError> {
        let mut cmd = cmd.clone();
        cmd.env
            .push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
        cmd.env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
        self.0.run(&cmd)
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-C", dir.to_str().unwrap()])
        .args(args)
        .envs([
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-m", file]);
}

struct Repo {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    worktrees: PathBuf,
    folder: PathBuf,
    spec: WorkerSpec,
}

/// The owner's repository on `main` and a worker's worktree on its own branch, with one
/// commit of the worker that writes `file`.
fn repo(file: &str) -> Repo {
    let temp = tempfile::tempdir().unwrap();
    let (repo, worktrees) = (temp.path().join("repo"), temp.path().join("worktrees"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&worktrees).unwrap();
    git(&repo, &["init", "-b", "main"]);
    for kv in [
        ["user.name", "T"],
        ["user.email", "t@test.local"],
        ["commit.gpgsign", "false"],
    ] {
        git(&repo, &["config", kv[0], kv[1]]);
    }
    commit(&repo, "file.txt", "one\n");
    let new = NewWorktree {
        repo: &repo,
        project: "app",
        task_id: "t",
        run_id: 1,
        base_branch: "main",
    };
    let info = create_worktree(&IsolatedGit(OsCommands::new()), &worktrees, &new, 1).unwrap();
    commit(&info.folder, file, "worker\n");
    std::fs::write(
        PathBuf::from(format!("{}.brief.md", info.folder.display())),
        "b",
    )
    .unwrap();
    let tip = git(&info.folder, &["rev-parse", "HEAD"]);
    let spec = WorkerSpec {
        repo: repo.display().to_string(),
        tip: Some(tip),
        worktree: Some(info.folder.display().to_string()),
        branch: Some(info.branch),
        ..Default::default()
    };
    Repo {
        _temp: temp,
        repo,
        worktrees,
        folder: info.folder,
        spec,
    }
}

fn act(r: &Repo, action: WorkerAction) -> Result<String, String> {
    let spec = WorkerSpec {
        action,
        ..r.spec.clone()
    };
    review(&IsolatedGit(OsCommands::new()), &r.worktrees, &spec)
}

fn branch_exists(r: &Repo) -> bool {
    let branch = r.spec.branch.clone().unwrap();
    !git(&r.repo, &["branch", "--list", &branch]).is_empty()
}

#[test]
fn keep_removes_the_folder_and_discard_the_branch_too_unless_work_is_left() {
    let r = repo("new.txt");
    act(&r, WorkerAction::Keep).unwrap();
    assert!(!r.folder.exists());
    assert!(branch_exists(&r));
    assert!(!PathBuf::from(format!("{}.brief.md", r.folder.display())).exists());

    let r = repo("new.txt");
    std::fs::write(r.folder.join("draft.txt"), "wip").unwrap();
    let err = act(&r, WorkerAction::Discard).unwrap_err();
    assert!(err.contains("draft.txt"), "{err}");
    assert!(r.folder.join("draft.txt").exists());
    assert!(branch_exists(&r));

    std::fs::remove_file(r.folder.join("draft.txt")).unwrap();
    act(&r, WorkerAction::Discard).unwrap();
    assert!(!r.folder.exists());
    assert!(!branch_exists(&r));
}

#[test]
fn merge_refuses_a_dirty_checkout_and_a_conflict_and_merges_a_clean_branch() {
    let r = repo("new.txt");
    std::fs::write(r.repo.join("file.txt"), "owner's edit\n").unwrap();
    let head = git(&r.repo, &["rev-parse", "HEAD"]);
    let err = act(&r, WorkerAction::KeepMerge).unwrap_err();
    assert!(err.contains("uncommitted"), "{err}");
    assert_eq!(git(&r.repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&r.repo, &["status", "--porcelain"]), "M file.txt");
    assert!(r.folder.exists());

    let r = repo("file.txt");
    commit(&r.repo, "file.txt", "owner\n");
    let head = git(&r.repo, &["rev-parse", "HEAD"]);
    let err = act(&r, WorkerAction::KeepMerge).unwrap_err();
    assert!(err.contains("conflicts"), "{err}");
    assert_eq!(git(&r.repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&r.repo, &["status", "--porcelain"]), "");

    let r = repo("new.txt");
    act(&r, WorkerAction::KeepMerge).unwrap();
    assert!(r.repo.join("new.txt").exists());
    assert!(!r.folder.exists());
}

#[test]
fn a_merge_that_fails_after_it_started_is_aborted() {
    let r = repo("new.txt");
    commit(&r.repo, "other.txt", "owner\n");
    // idlemax's git runs no hook, so a stale index lock makes the started merge fail.
    std::fs::write(r.repo.join(".git/index.lock"), "").unwrap();
    let head = git(&r.repo, &["rev-parse", "HEAD"]);
    let err = act(&r, WorkerAction::KeepMerge).unwrap_err();
    assert!(err.contains("aborted"), "{err}");
    assert_eq!(git(&r.repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&r.repo, &["status", "--porcelain"]), "");
    assert!(r.folder.exists());
}

/// Red-team case 6: keep --merge merges the commit the check judged, never a branch moved since.
#[test]
fn merge_takes_the_checked_tip_and_refuses_a_branch_moved_since_or_no_tip() {
    let r = repo("new.txt");
    commit(&r.folder, "later.txt", "after the check\n");
    let head = git(&r.repo, &["rev-parse", "HEAD"]);
    let err = act(&r, WorkerAction::KeepMerge).unwrap_err();
    assert!(err.contains("moved since its check"), "{err}");
    assert_eq!(git(&r.repo, &["rev-parse", "HEAD"]), head);

    let mut r = repo("new.txt");
    r.spec.tip = None;
    let err = act(&r, WorkerAction::KeepMerge).unwrap_err();
    assert!(err.contains("no checked commit"), "{err}");

    let r = repo("new.txt");
    let tip = r.spec.tip.clone().unwrap();
    act(&r, WorkerAction::KeepMerge).unwrap();
    assert_eq!(git(&r.repo, &["rev-parse", "HEAD"]), tip);
}
