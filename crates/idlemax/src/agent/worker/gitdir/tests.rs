//! Tests: red-team case 5 in a temp repository with real git: a worker that writes the shared
//! `.git` (core.fsmonitor, hooks) never gets idlemax's git to run its code, and the snapshot
//! finds and undoes the change.

use std::path::{Path, PathBuf};

use idlemax_core::worker::{WorkerAction, WorkerSpec};

use super::super::cmd::{Cmd, CmdError, Commands, OsCommands, Output};
use super::super::review::review;
use super::super::worktree::{create_worktree, NewWorktree};

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

/// A shell in `dir`, as the worker's own shell runs the red-team's commands.
fn sh(dir: &Path, script: &str) {
    let out = std::process::Command::new("sh")
        .args(["-c", script])
        .current_dir(dir)
        .envs([
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{script}: {out:?}");
}

struct Rig {
    temp: tempfile::TempDir,
    repo: PathBuf,
    worktrees: PathBuf,
    folder: PathBuf,
    branch: String,
}

/// The owner's repository on `main` and a worker's worktree with one commit.
fn rig() -> Rig {
    let temp = tempfile::tempdir().unwrap();
    let (repo, worktrees) = (temp.path().join("repo"), temp.path().join("worktrees"));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&worktrees).unwrap();
    sh(
        &repo,
        "git init -q -b main && git config user.name T && git config user.email t@test.local \
         && git config commit.gpgsign false && echo one > file.txt && git add file.txt \
         && git commit -qm base",
    );
    let info = worktree(&repo, &worktrees, "t");
    sh(
        &info.0,
        "echo new > new.txt && git add new.txt && git commit -qm new",
    );
    Rig {
        temp,
        repo,
        worktrees,
        folder: info.0,
        branch: info.1,
    }
}

fn worktree(repo: &Path, worktrees: &Path, task: &str) -> (PathBuf, String) {
    let new = NewWorktree {
        repo,
        project: "app",
        task_id: task,
        run_id: 1,
        base_branch: "main",
    };
    let info = create_worktree(&IsolatedGit(OsCommands::new()), worktrees, &new, 1).unwrap();
    (info.folder, info.branch)
}

/// The red-team's commands, run in the worker's worktree.
fn attack(r: &Rig) {
    let t = r.temp.path().display();
    sh(
        &r.folder,
        &format!("git config core.fsmonitor 'touch {t}/pwned-fsmonitor; false #'"),
    );
    for hook in ["post-checkout", "post-merge"] {
        sh(
            &r.folder,
            &format!(
                "h=$(git rev-parse --git-common-dir)/hooks/{hook} && \
                 printf '#!/bin/sh\\ntouch {t}/pwned-hook-{hook}\\n' > \"$h\" && chmod +x \"$h\""
            ),
        );
    }
}

fn pwned(r: &Rig) -> Vec<String> {
    std::fs::read_dir(r.temp.path())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("pwned"))
        .collect()
}

#[test]
fn idlemax_git_never_runs_a_workers_fsmonitor_or_hooks() {
    let r = rig();
    attack(&r);
    worktree(&r.repo, &r.worktrees, "next");
    let tip = String::from_utf8(
        std::process::Command::new("git")
            .args(["-C", r.folder.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let spec = WorkerSpec {
        action: WorkerAction::KeepMerge,
        repo: r.repo.display().to_string(),
        worktree: Some(r.folder.display().to_string()),
        branch: Some(r.branch.clone()),
        tip: Some(tip.trim().to_string()),
        ..Default::default()
    };
    review(&IsolatedGit(OsCommands::new()), &r.worktrees, &spec).unwrap();
    assert!(r.repo.join("new.txt").exists());
    assert_eq!(pwned(&r), Vec::<String>::new());
}

#[test]
fn a_change_to_the_shared_config_or_hooks_is_found_and_put_back() {
    let r = rig();
    let common = r.repo.join(".git");
    let config = std::fs::read(common.join("config")).unwrap();
    let copy = r.temp.path().join("snapshot");
    let snap = super::take(&IsolatedGit(OsCommands::new()), &r.folder, &copy).unwrap();
    let real = |p: &Path| std::fs::canonicalize(p).unwrap();
    assert_eq!(real(&snap.common), real(&common));
    assert!(!super::undo_change(&snap));
    attack(&r);
    assert!(super::undo_change(&snap));
    assert_eq!(std::fs::read(common.join("config")).unwrap(), config);
    assert!(!common.join("hooks/post-checkout").exists());
    assert!(!common.join("hooks/post-merge").exists());
    assert!(!super::undo_change(&snap));
    super::discard(&snap);
    assert!(!copy.exists());
}

#[test]
fn a_change_to_the_worktrees_git_pointers_is_found_and_put_back() {
    // Review finding 4: the worktree's `.git` file and its admin dir's `commondir` decide
    // which repository idlemax's own git reads; a worker without the sandbox can rewrite them.
    let r = rig();
    let copy = r.temp.path().join("snap");
    let snap = super::take(&IsolatedGit(OsCommands::new()), &r.folder, &copy).unwrap();
    let gitfile = std::fs::read_to_string(r.folder.join(".git")).unwrap();
    let admin = r
        .repo
        .join(".git/worktrees")
        .join(r.folder.file_name().unwrap());
    let commondir = std::fs::read_to_string(admin.join("commondir")).unwrap();
    sh(
        &r.folder,
        "git --git-dir=.evil init -q && printf 'gitdir: %s/.evil\\n' \"$PWD\" > .git",
    );
    std::fs::write(admin.join("commondir"), "/tmp\n").unwrap();
    assert!(super::undo_change(&snap));
    assert_eq!(
        std::fs::read_to_string(r.folder.join(".git")).unwrap(),
        gitfile
    );
    assert_eq!(
        std::fs::read_to_string(admin.join("commondir")).unwrap(),
        commondir
    );
    assert!(!super::undo_change(&snap), "nothing changed since");
}
