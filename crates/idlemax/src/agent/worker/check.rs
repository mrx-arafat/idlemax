//! Adapter: gathers the facts of a finished worker's worktree for the verdict of D8, on a
//! thread of its own: the commits since the base, what is left uncommitted or untracked, the
//! diff per file, the last commit's subject, the project's check before and after, and on a
//! high-risk diff the answer of a reviewer from another provider. idlemax runs only the
//! project's own check; the task's done check is never executed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use idlemax_core::types::{JobId, QuotaSnapshot};
use idlemax_core::worker::checks::{
    parse_review, review_prompt, risk_is_high, MAX_REVIEW_DIFF_BYTES, REVIEW_SCHEMA,
};
use idlemax_core::worker::launch::reviewer_args;
use idlemax_core::worker::report::{CheckFacts, DiffStat, FileDiff, ReviewFacts};
use idlemax_core::worker::{Reviewer, WorkerCli, WorkerSpec};

use super::cmd::{Cmd, Commands};
use super::gitdir::{self, GitSnapshot};
use super::steps::{run_project, Fence};
use super::worktree;

const GIT_LIMIT: Duration = Duration::from_secs(10);
const REVIEW_LIMIT: Duration = Duration::from_secs(300);

pub struct CheckCtx {
    pub commands: Arc<dyn Commands>,
    pub worker: WorkerSpec,
    pub job: JobId,
    pub tag: u64,
    pub folder: PathBuf,
    /// The Run's own branch: the check judges it, never whatever HEAD points at.
    pub branch: String,
    pub base_commit: String,
    /// The repository's git config and hooks before the launch, compared again once the
    /// project's check and the reviewer have run.
    pub git: Option<GitSnapshot>,
    /// They had already changed when the worker ended.
    pub git_changed: bool,
    pub remotes: Vec<String>,
    /// The agent's newest quota readings, so a review counts for the account that paid.
    pub readings: Arc<Mutex<Vec<QuotaSnapshot>>>,
    pub cancel: Arc<AtomicBool>,
    /// The state dir and the digests of its guarded files when the launch began.
    pub state_dir: PathBuf,
    pub guarded: Vec<(String, String)>,
}

pub fn spawn(ctx: CheckCtx) -> Receiver<CheckFacts> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(gather(&ctx));
    });
    rx
}

fn gather(ctx: &CheckCtx) -> CheckFacts {
    // A sandboxed worker committed in its private git dir; only its branch comes over.
    let imported = super::sandbox::private_git::import(
        &ctx.state_dir,
        ctx.worker.run,
        &ctx.folder,
        &ctx.branch,
    );
    if let Err(e) = imported {
        tracing::warn!(job = ctx.job, error = %e, "the sandboxed worker's branch was not imported");
    }
    let (tip, head_off_branch) = branch_tip(ctx);
    // A branch that is gone has nothing to judge: the empty range finds no commit.
    let target = tip.clone().unwrap_or_else(|| ctx.base_commit.clone());
    let range = format!("{}..{target}", ctx.base_commit);
    let commits = git(ctx, &["rev-list", "--count", &range])
        .and_then(|out| out.trim().parse().ok())
        .unwrap_or(0);
    let (modified, untracked) =
        porcelain(&git(ctx, &["status", "--porcelain", "-z", "--no-renames"]).unwrap_or_default());
    let numstat_args = ["diff", "--numstat", "-z", "--no-renames", &range];
    let diff = numstat(&git(ctx, &numstat_args).unwrap_or_default());
    let summary = (commits > 0)
        .then(|| git(ctx, &["log", "-1", "--format=%s", &target]))
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let baseline_before = worktree::marker_of(&ctx.folder).and_then(|m| m.baseline);
    let baseline_after = match ctx.worker.check.is_empty() || cancelled(ctx) {
        true => None,
        false => check_after(ctx),
    };
    let mut facts = CheckFacts {
        commits,
        tip,
        head_off_branch,
        git_changed: false,
        modified,
        untracked,
        diff,
        summary,
        baseline_before,
        baseline_after,
        review: None,
        // idlemax's own files the turn changed without an owner command (red team 2, 3).
        tampered: crate::owner_writes::tampered_since(&ctx.state_dir, &ctx.guarded),
    };
    let w = &ctx.worker;
    let high = risk_is_high(
        &facts.diff,
        w.review_diff_lines,
        &w.core_paths,
        !w.check.is_empty(),
        baseline_before,
    );
    if let (true, Some(reviewer), false) = (high, &w.reviewer, cancelled(ctx)) {
        facts.review = Some(review(ctx, reviewer, &range));
    }
    let changed_now = ctx.git.as_ref().is_some_and(gitdir::undo_change);
    facts.git_changed = ctx.git_changed || changed_now;
    facts
}

/// The commit of the Run's branch, and whether the worktree's HEAD is anything but that
/// branch at that commit: a worker could commit on the branch and move HEAD elsewhere.
fn branch_tip(ctx: &CheckCtx) -> (Option<String>, bool) {
    let full = format!("refs/heads/{}", ctx.branch);
    let tip = git(
        ctx,
        &["rev-parse", "--verify", "-q", &format!("{full}^{{commit}}")],
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !ctx.branch.is_empty() && !s.is_empty());
    let on_branch = git(ctx, &["symbolic-ref", "-q", "HEAD"]).is_some_and(|r| r.trim() == full);
    let head = git(ctx, &["rev-parse", "--verify", "-q", "HEAD"]).map(|s| s.trim().to_string());
    let off = !on_branch || tip.is_none() || head != tip;
    (tip, off)
}

fn check_after(ctx: &CheckCtx) -> Option<i32> {
    let fence = Fence {
        job: ctx.job,
        tag: ctx.tag,
        remotes: &ctx.remotes,
    };
    let secs = ctx.worker.check_secs;
    run_project(
        ctx.commands.as_ref(),
        &fence,
        &ctx.folder,
        &ctx.worker.check,
        secs,
    )
    .ok()?
}

/// One review by a different provider; a diff too large to send counts as not reviewed.
fn review(ctx: &CheckCtx, r: &Reviewer, range: &str) -> ReviewFacts {
    let mut facts = ReviewFacts {
        cli: r.cli,
        account: r.account.clone(),
        model: r.model.clone(),
        quota_before: readings_of(ctx, &r.account),
        ..Default::default()
    };
    let diff = git(ctx, &["diff", range]).unwrap_or_default();
    if diff.len() > MAX_REVIEW_DIFF_BYTES {
        facts.concern = Some("the diff is too large to send to a reviewer".into());
        return facts;
    }
    if let Some((answer, concern)) = ask(ctx, r, &diff) {
        facts.answer = Some(answer);
        facts.concern = concern;
    }
    facts.quota_after = readings_of(ctx, &r.account);
    facts
}

fn ask(
    ctx: &CheckCtx,
    r: &Reviewer,
    diff: &str,
) -> Option<(idlemax_core::worker::report::ReviewAnswer, Option<String>)> {
    let schema_file = side_file(&ctx.folder, "review-schema.json");
    let out_file = side_file(&ctx.folder, "review-answer.json");
    std::fs::write(&schema_file, REVIEW_SCHEMA).ok()?;
    let (schema, out) = (
        schema_file.display().to_string(),
        out_file.display().to_string(),
    );
    let argv = reviewer_args(r.cli, &r.model, &schema, &out).ok()?;
    let (program, args) = argv.split_first()?;
    let cmd = Cmd {
        program: program.clone(),
        args: args.to_vec(),
        cwd: Some(ctx.folder.clone()),
        env: Vec::new(),
        stdin: Some(review_prompt(&ctx.worker, diff)),
        limit: REVIEW_LIMIT,
        throttle: false,
        tag: None,
    };
    let output = ctx.commands.run(&cmd).ok().filter(|o| o.code == Some(0))?;
    let json = match r.cli {
        WorkerCli::Codex => serde_json::from_str(&std::fs::read_to_string(&out_file).ok()?).ok()?,
        _ => crate::advisor::claude::structured_output(&output.stdout).ok()?,
    };
    let _ = std::fs::remove_file(&out_file);
    parse_review(&json)
}

/// The newest readings of `account`, per window.
fn readings_of(ctx: &CheckCtx, account: &str) -> Vec<QuotaSnapshot> {
    let all = ctx.readings.lock().unwrap_or_else(PoisonError::into_inner);
    all.iter()
        .filter(|s| s.account == account)
        .cloned()
        .collect()
}

/// Modified tracked paths and new untracked paths from `git status --porcelain -z`. Paths
/// come unquoted; a rename, if one still shows, counts as both its new and its old path.
fn porcelain(text: &str) -> (Vec<String>, Vec<String>) {
    let (mut modified, mut untracked) = (Vec::new(), Vec::new());
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        if record.len() <= 3 {
            continue;
        }
        let (status, path) = record.split_at(3);
        match status.trim() {
            "??" => untracked.push(path.to_string()),
            s => {
                modified.push(path.to_string());
                let source = match s.contains('R') || s.contains('C') {
                    true => records.next(),
                    false => None,
                };
                if let (true, Some(old)) = (s.contains('R'), source) {
                    modified.push(old.to_string());
                }
            }
        }
    }
    (modified, untracked)
}

/// `git diff --numstat -z` records; a binary file (`-`) counts no lines. A rename, if one
/// still shows (an empty path, then the old and the new), counts as both paths.
fn numstat(text: &str) -> DiffStat {
    let mut files = Vec::new();
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (added, removed) = (added.parse().unwrap_or(0), removed.parse().unwrap_or(0));
        let paths = match path.is_empty() {
            true => records.by_ref().take(2).map(str::to_string).collect(),
            false => vec![path.to_string()],
        };
        for path in paths {
            files.push(FileDiff {
                path,
                added,
                removed,
            });
        }
    }
    DiffStat { files }
}

fn git(ctx: &CheckCtx, args: &[&str]) -> Option<String> {
    let out = ctx
        .commands
        .run(&Cmd::git(&ctx.folder, args, GIT_LIMIT))
        .ok()?;
    (out.code == Some(0)).then_some(out.stdout)
}

fn cancelled(ctx: &CheckCtx) -> bool {
    ctx.cancel.load(Ordering::SeqCst)
}

/// `<folder>.<name>`, beside the worktree and outside the repository.
fn side_file(folder: &Path, name: &str) -> PathBuf {
    PathBuf::from(format!("{}.{name}", folder.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::worker::cmd::{CmdError, OsCommands, Output};
    use idlemax_core::worker::checks::verdict;
    use idlemax_core::worker::run::Verdict;

    #[test]
    fn porcelain_and_numstat_are_read() {
        let (modified, untracked) = porcelain(" M src/a.rs\0R  new.rs\0old.rs\0?? notes.txt\0");
        assert_eq!(modified, ["src/a.rs", "new.rs", "old.rs"]);
        assert_eq!(untracked, ["notes.txt"]);
        let diff = numstat("3\t1\tsrc/a.rs\0-\t-\tlogo.png\0");
        assert_eq!(diff.lines(), 4);
        assert_eq!(diff.files[1].path, "logo.png");
    }

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

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@test.local"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .envs([
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn a_rename_out_of_an_off_limits_folder_is_rejected_and_a_non_ascii_path_is_read() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git_in(dir, &["init", "-b", "main"]);
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for f in ["secret/key.txt", "secret/other.txt"] {
            std::fs::write(dir.join(f), format!("{f} stays the same\n")).unwrap();
        }
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-m", "base"]);
        let base = git_in(dir, &["rev-parse", "HEAD"]);
        git_in(dir, &["mv", "secret/key.txt", "src/key.txt"]);
        std::fs::write(dir.join("src/café.txt"), "new\n").unwrap();
        git_in(dir, &["add", "src/café.txt"]);
        git_in(dir, &["commit", "-m", "move the key"]);
        git_in(dir, &["mv", "secret/other.txt", "src/other.txt"]);
        let ctx = CheckCtx {
            commands: Arc::new(IsolatedGit(OsCommands::new())),
            worker: WorkerSpec {
                scope_paths: vec!["src/".into()],
                off_limits: vec!["secret/".into()],
                ..Default::default()
            },
            job: 1,
            tag: 0,
            folder: dir.to_path_buf(),
            branch: "main".into(),
            base_commit: base,
            git: None,
            git_changed: false,
            remotes: Vec::new(),
            readings: Arc::default(),
            cancel: Arc::default(),
            state_dir: PathBuf::new(),
            guarded: Vec::new(),
        };
        let facts = gather(&ctx);
        let paths: Vec<&str> = facts.diff.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/café.txt"), "{paths:?}");
        assert!(paths.contains(&"secret/key.txt"), "{paths:?}");
        assert!(facts.modified.contains(&"secret/other.txt".to_string()));
        let judged = verdict(&facts, &ctx.worker);
        assert_eq!(judged.verdict, Verdict::Rejected);
        let concern = judged.concern.unwrap_or_default();
        assert!(concern.contains("secret/key.txt"), "{concern}");
        assert!(concern.contains("secret/other.txt"), "{concern}");
    }

    /// Red-team case 6: commit off-limits work on the Run's branch, then point HEAD elsewhere.
    #[test]
    fn the_check_judges_the_runs_branch_and_rejects_a_head_moved_off_it() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git_in(dir, &["init", "-b", "main"]);
        std::fs::create_dir_all(dir.join("secrets")).unwrap();
        std::fs::write(dir.join("secrets/key"), "key\n").unwrap();
        std::fs::write(dir.join("file.txt"), "one\n").unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-m", "base"]);
        let base = git_in(dir, &["rev-parse", "HEAD"]);
        git_in(dir, &["checkout", "-q", "-b", "idlemax/task-1"]);
        std::fs::write(dir.join("secrets/key"), "stolen\n").unwrap();
        git_in(dir, &["commit", "-qam", "off-limits edit"]);
        let tip = git_in(dir, &["rev-parse", "HEAD"]);
        git_in(dir, &["checkout", "-q", "--detach", &base]);
        std::fs::write(dir.join("file.txt"), "one\nok\n").unwrap();
        git_in(dir, &["commit", "-qam", "in-scope edit"]);
        let ctx = CheckCtx {
            commands: Arc::new(IsolatedGit(OsCommands::new())),
            worker: WorkerSpec {
                scope_paths: vec!["file.txt".into()],
                off_limits: vec!["secrets/".into()],
                ..Default::default()
            },
            job: 1,
            tag: 0,
            folder: dir.to_path_buf(),
            branch: "idlemax/task-1".into(),
            base_commit: base,
            git: None,
            git_changed: false,
            remotes: Vec::new(),
            readings: Arc::default(),
            cancel: Arc::default(),
            state_dir: PathBuf::new(),
            guarded: Vec::new(),
        };
        let facts = gather(&ctx);
        assert_eq!(facts.tip.as_deref(), Some(tip.as_str()));
        assert!(facts.head_off_branch);
        let paths: Vec<&str> = facts.diff.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["secrets/key"]);
        let judged = verdict(&facts, &ctx.worker);
        assert_eq!(judged.verdict, Verdict::Rejected);
        let concern = judged.concern.unwrap_or_default();
        assert!(concern.contains("HEAD is not"), "{concern}");
        assert!(concern.contains("secrets/key"), "{concern}");
    }
}
